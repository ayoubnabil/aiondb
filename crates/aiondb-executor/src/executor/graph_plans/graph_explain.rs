//! Cypher graph EXPLAIN / plan-description rendering.
//!
//! Split out of `graph_plans/mod.rs` (see the module docs there). Pure
//! summary/severity/JSON line builders plus the `Executor` methods that
//! describe Cypher pattern/query graph plans. Shared types and core
//! helpers stay in the parent module, reached via `use super::*`
//! (matching the sibling-module convention, e.g. `graph_mutate.rs`).
#![allow(clippy::too_many_lines)]

use super::*;

fn explain_cypher_node_pattern(node: &CypherNodePattern) -> String {
    let mut rendered = String::from("(");
    if let Some(variable) = node.variable.as_deref() {
        rendered.push_str(variable);
    }
    if let Some(label) = node.label.as_deref() {
        rendered.push(':');
        rendered.push_str(label);
    }
    if !node.properties.is_empty() {
        rendered.push_str(" {");
        for (index, property) in node.properties.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&property.key);
        }
        rendered.push('}');
    }
    rendered.push(')');
    rendered
}

fn explain_cypher_relationship_pattern(rel: &CypherRelPattern) -> String {
    let mut rendered = String::from("[");
    if let Some(variable) = rel.variable.as_deref() {
        rendered.push_str(variable);
    }
    if let Some(rel_type) = rel.rel_type.as_deref() {
        rendered.push(':');
        rendered.push_str(rel_type);
    } else if !rel.rel_type_alternatives.is_empty() {
        rendered.push(':');
        rendered.push_str(&rel.rel_type_alternatives.join("|"));
    }
    if rel.min_hops.is_some() || rel.max_hops.is_some() {
        rendered.push('*');
        if let Some(min) = rel.min_hops {
            rendered.push_str(&min.to_string());
        }
        rendered.push_str("..");
        if let Some(max) = rel.max_hops {
            rendered.push_str(&max.to_string());
        }
    }
    if !rel.properties.is_empty() {
        rendered.push_str(" {");
        for (index, property) in rel.properties.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&property.key);
        }
        rendered.push('}');
    }
    rendered.push(']');
    rendered
}

fn explain_cypher_pattern_shape(pattern: &CypherPattern) -> String {
    let mut rendered = String::new();
    if let Some(path_variable) = pattern.path_variable.as_deref() {
        rendered.push_str(path_variable);
        rendered.push_str(" = ");
    }
    for (index, node) in pattern.nodes.iter().enumerate() {
        if index > 0 {
            let rel = &pattern.relationships[index - 1];
            match rel.direction {
                CypherRelDirection::Outgoing => {
                    rendered.push('-');
                    rendered.push_str(&explain_cypher_relationship_pattern(rel));
                    rendered.push_str("->");
                }
                CypherRelDirection::Incoming => {
                    rendered.push_str("<-");
                    rendered.push_str(&explain_cypher_relationship_pattern(rel));
                    rendered.push('-');
                }
                CypherRelDirection::Both => {
                    rendered.push('-');
                    rendered.push_str(&explain_cypher_relationship_pattern(rel));
                    rendered.push('-');
                }
            }
        }
        rendered.push_str(&explain_cypher_node_pattern(node));
    }
    rendered
}

fn explain_cypher_pattern_bound_vars(pattern: &CypherPattern) -> String {
    let mut vars = Vec::new();
    if let Some(path_variable) = pattern.path_variable.as_deref() {
        vars.push(path_variable.to_owned());
    }
    for node in &pattern.nodes {
        if let Some(variable) = node.variable.as_deref() {
            vars.push(variable.to_owned());
        }
    }
    for rel in &pattern.relationships {
        if let Some(variable) = rel.variable.as_deref() {
            vars.push(variable.to_owned());
        }
    }
    if vars.is_empty() {
        "none".to_owned()
    } else {
        vars.join(",")
    }
}

fn collect_cypher_pattern_bound_vars(pattern: &CypherPattern) -> HashSet<String> {
    let mut vars = HashSet::new();
    if let Some(path_variable) = pattern.path_variable.as_deref() {
        vars.insert(path_variable.to_owned());
    }
    for node in &pattern.nodes {
        if let Some(variable) = node.variable.as_deref() {
            vars.insert(variable.to_owned());
        }
    }
    for rel in &pattern.relationships {
        if let Some(variable) = rel.variable.as_deref() {
            vars.insert(variable.to_owned());
        }
    }
    vars
}

fn explain_cypher_pattern_flags(pattern: &CypherPattern) -> String {
    let has_var_length = pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some());
    let has_both_direction = pattern
        .relationships
        .iter()
        .any(|rel| rel.direction == CypherRelDirection::Both);
    let has_relationship_alternatives = pattern
        .relationships
        .iter()
        .any(|rel| !rel.rel_type_alternatives.is_empty());
    let uses_named_path = pattern.path_variable.is_some();
    let uses_path_function = pattern.path_function.is_some();

    format!(
        "named_path={}, path_function={}, var_length={}, both_direction={}, rel_alternatives={}",
        uses_named_path,
        uses_path_function,
        has_var_length,
        has_both_direction,
        has_relationship_alternatives,
    )
}

fn explain_cypher_pattern_seed(pattern: &CypherPattern) -> String {
    pattern
        .nodes
        .first()
        .map(explain_cypher_node_pattern)
        .unwrap_or_else(|| "unknown".to_owned())
}

fn explain_cypher_pattern_seed_constraints(pattern: &CypherPattern) -> String {
    let Some(seed) = pattern.nodes.first() else {
        return "unknown".to_owned();
    };
    let mut parts = Vec::new();
    if let Some(label) = seed.label.as_deref() {
        parts.push(format!("label={label}"));
    }
    if !seed.properties.is_empty() {
        parts.push(format!(
            "properties={}",
            seed.properties
                .iter()
                .map(|property| property.key.clone())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if seed.index_scan.is_some() {
        parts.push("index_scan=true".to_owned());
    }
    if !seed.range_pushdown.is_empty() {
        parts.push(format!("range_pushdown={}", seed.range_pushdown.len()));
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(";")
    }
}

fn explain_cypher_pattern_seed_binding_mode(pattern: &CypherPattern) -> String {
    let Some(seed) = pattern.nodes.first() else {
        return "unknown".to_owned();
    };
    let has_id_property = seed
        .properties
        .iter()
        .any(|property| property.key.eq_ignore_ascii_case("id"));
    let has_index_scan = seed.index_scan.is_some();
    let has_range_pushdown = !seed.range_pushdown.is_empty();

    if has_id_property {
        "id_constrained".to_owned()
    } else if has_index_scan {
        "indexed".to_owned()
    } else if has_range_pushdown {
        "range_constrained".to_owned()
    } else if seed.variable.is_some() {
        "label_scan".to_owned()
    } else {
        "anonymous_scan".to_owned()
    }
}

const INFERRED_SOURCE: &str = "inferred";

struct PatternStaticExplainFields {
    seed: String,
    seed_binding_state: String,
    correlated_vars: String,
    seed_mode: String,
    seed_constraints: String,
    first_rel: String,
    first_rel_mode: String,
    first_rel_constraints: String,
    bound_vars: String,
    flags: String,
    shape: String,
}

impl PatternStaticExplainFields {
    fn gather(pattern: &CypherPattern, available_vars: &HashSet<String>) -> Self {
        Self {
            seed: explain_cypher_pattern_seed(pattern),
            seed_binding_state: explain_cypher_pattern_seed_binding_state(pattern, available_vars),
            correlated_vars: explain_cypher_pattern_correlated_vars(pattern, available_vars),
            seed_mode: explain_cypher_pattern_seed_binding_mode(pattern),
            seed_constraints: explain_cypher_pattern_seed_constraints(pattern),
            first_rel: explain_cypher_pattern_first_relationship(pattern),
            first_rel_mode: explain_cypher_pattern_first_relationship_mode(pattern),
            first_rel_constraints: explain_cypher_pattern_first_relationship_constraints(pattern),
            bound_vars: explain_cypher_pattern_bound_vars(pattern),
            flags: explain_cypher_pattern_flags(pattern),
            shape: explain_cypher_pattern_shape(pattern),
        }
    }
}

fn explain_cypher_pattern_runtime_pivot_index(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
) -> Option<usize> {
    let context = ExecutionContext {
        txn_id,
        ..ExecutionContext::default()
    };
    executor
        .cbo_seed_index_for_pattern(&context, pattern)
        .ok()
        .flatten()
        .or_else(|| pick_match_pivot_index(pattern))
}

fn explain_cypher_pattern_pivot_driver(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
) -> String {
    if pattern.nodes.len() <= 1 {
        return "single_node".to_owned();
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return "blocked".to_owned();
    }
    let context = ExecutionContext {
        txn_id,
        ..ExecutionContext::default()
    };
    if executor
        .cbo_seed_index_for_pattern(&context, pattern)
        .ok()
        .flatten()
        .is_some()
    {
        "cbo".to_owned()
    } else if pick_match_pivot_index(pattern).is_some() {
        "heuristic".to_owned()
    } else {
        "leftmost".to_owned()
    }
}

fn explain_cypher_pattern_pivot_reason(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
) -> String {
    if pattern.nodes.len() <= 1 {
        return "single_node_pattern".to_owned();
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return "leftmost_walk_required_for_var_length".to_owned();
    }
    match explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern) {
        Some(pivot) => {
            let pivot_mode = explain_cypher_pattern_seed_binding_mode(&CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![pattern.nodes[pivot].clone()],
                relationships: vec![],
            });
            format!("pivot_to_node_{pivot}:{pivot_mode}")
        }
        None => "leftmost_seed_retained".to_owned(),
    }
}

fn explain_cypher_pattern_pivot_scores(pattern: &CypherPattern) -> String {
    if pattern.nodes.is_empty() {
        return "none".to_owned();
    }
    pattern
        .nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let mode = explain_cypher_pattern_seed_binding_mode(&CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![node.clone()],
                relationships: vec![],
            });
            format!("{idx}:{mode}:{}", pivot_node_score(node))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn explain_cypher_pattern_pivot_decision(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
) -> String {
    if pattern.nodes.len() <= 1 {
        return "single_node_pattern".to_owned();
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return "var_length_blocks_pivot".to_owned();
    }
    match explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern) {
        Some(pivot) => format!("selected_node_{pivot}"),
        None => "retained_leftmost".to_owned(),
    }
}

fn explain_cypher_pattern_pivot_margin(pattern: &CypherPattern) -> String {
    if pattern.nodes.len() <= 1 {
        return "none".to_owned();
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return "blocked".to_owned();
    }
    let Some(leftmost) = pattern.nodes.first() else {
        return "unknown".to_owned();
    };
    let leftmost_score = pivot_node_score(leftmost);
    let Some(best_score) = pattern.nodes.iter().map(pivot_node_score).min() else {
        return "unknown".to_owned();
    };
    format!("{}", leftmost_score.saturating_sub(best_score))
}

fn explain_cypher_pattern_pivot_competition(pattern: &CypherPattern) -> String {
    if pattern.nodes.len() <= 1 {
        return "none".to_owned();
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return "blocked".to_owned();
    }

    let mut scored = pattern
        .nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (idx, pivot_node_score(node)))
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return "unknown".to_owned();
    }
    scored.sort_by_key(|(idx, score)| (*score, *idx));
    let (winner_idx, winner_score) = scored[0];
    let (runner_up_idx, runner_up_score) = scored
        .get(1)
        .copied()
        .unwrap_or((winner_idx, winner_score));
    format!(
        "winner={winner_idx}:{winner_score},runner_up={runner_up_idx}:{runner_up_score},delta={}",
        runner_up_score.saturating_sub(winner_score)
    )
}

fn cypher_pattern_pivot_delta(pattern: &CypherPattern) -> Option<u8> {
    if pattern.nodes.len() <= 1 {
        return None;
    }
    if pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
    {
        return None;
    }
    let mut scored = pattern
        .nodes
        .iter()
        .map(pivot_node_score)
        .collect::<Vec<_>>();
    if scored.len() < 2 {
        return None;
    }
    scored.sort_unstable();
    Some(scored[1].saturating_sub(scored[0]))
}

fn explain_cypher_pattern_seed_binding_state(
    pattern: &CypherPattern,
    available_vars: &HashSet<String>,
) -> String {
    let Some(seed) = pattern.nodes.first() else {
        return "unknown".to_owned();
    };
    let Some(variable) = seed.variable.as_deref() else {
        return "anonymous".to_owned();
    };
    if available_vars.contains(variable) {
        "prebound".to_owned()
    } else {
        "fresh".to_owned()
    }
}

fn explain_cypher_pattern_correlated_vars(
    pattern: &CypherPattern,
    available_vars: &HashSet<String>,
) -> String {
    let mut vars = Vec::new();
    if let Some(path_variable) = pattern.path_variable.as_deref() {
        if available_vars.contains(path_variable) {
            vars.push(path_variable.to_owned());
        }
    }
    for node in &pattern.nodes {
        if let Some(variable) = node.variable.as_deref() {
            if available_vars.contains(variable) {
                vars.push(variable.to_owned());
            }
        }
    }
    for rel in &pattern.relationships {
        if let Some(variable) = rel.variable.as_deref() {
            if available_vars.contains(variable) {
                vars.push(variable.to_owned());
            }
        }
    }
    if vars.is_empty() {
        "none".to_owned()
    } else {
        vars.join(",")
    }
}

fn explain_cypher_pattern_first_relationship(pattern: &CypherPattern) -> String {
    pattern
        .relationships
        .first()
        .map(explain_cypher_relationship_pattern)
        .unwrap_or_else(|| "none".to_owned())
}

fn explain_cypher_pattern_first_relationship_constraints(pattern: &CypherPattern) -> String {
    let Some(rel) = pattern.relationships.first() else {
        return "none".to_owned();
    };
    let mut parts = Vec::new();
    if let Some(rel_type) = rel.rel_type.as_deref() {
        parts.push(format!("type={rel_type}"));
    } else if !rel.rel_type_alternatives.is_empty() {
        parts.push(format!("types={}", rel.rel_type_alternatives.join("|")));
    }
    if !rel.properties.is_empty() {
        parts.push(format!(
            "properties={}",
            rel.properties
                .iter()
                .map(|property| property.key.clone())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if rel.index_scan.is_some() {
        parts.push("index_scan=true".to_owned());
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(";")
    }
}

fn explain_cypher_pattern_first_relationship_mode(pattern: &CypherPattern) -> String {
    let Some(rel) = pattern.relationships.first() else {
        return "none".to_owned();
    };
    if rel.min_hops.is_some() || rel.max_hops.is_some() {
        "var_length".to_owned()
    } else if rel.index_scan.is_some() {
        "indexed".to_owned()
    } else if !rel.properties.is_empty() {
        "property_filtered".to_owned()
    } else if !rel.rel_type_alternatives.is_empty() {
        "multi_type".to_owned()
    } else if rel.rel_type.is_some() {
        "typed_expand".to_owned()
    } else {
        "generic_expand".to_owned()
    }
}

fn explain_cypher_query_summary_line(query: &CypherQueryPlan) -> String {
    let pipeline_match_count = query
        .pipeline
        .iter()
        .filter(|op| matches!(op, CypherPipelineOp::Match(_)))
        .count();
    let pipeline_call_count = query
        .pipeline
        .iter()
        .filter(|op| matches!(op, CypherPipelineOp::CallSubquery(_)))
        .count();
    let pipeline_foreach_count = query
        .pipeline
        .iter()
        .filter(|op| matches!(op, CypherPipelineOp::Foreach(_)))
        .count();
    let top_level_pattern_count: usize = query.matches.iter().map(|clause| clause.patterns.len()).sum();
    let pipeline_pattern_count: usize = query
        .pipeline
        .iter()
        .filter_map(|op| match op {
            CypherPipelineOp::Match(clause) => Some(clause.patterns.len()),
            _ => None,
        })
        .sum();
    let optional_match_count = query
        .pipeline
        .iter()
        .filter_map(|op| match op {
            CypherPipelineOp::Match(clause) => Some(clause.optional),
            _ => None,
        })
        .chain(query.matches.iter().map(|clause| clause.optional))
        .filter(|optional| *optional)
        .count();
    let mut available_vars = HashSet::new();
    let mut correlated_pattern_count = 0usize;
    let mut var_length_pattern_count = 0usize;
    let mut named_path_count = 0usize;
    let mut both_direction_pattern_count = 0usize;
    let mut prebound_seed_count = 0usize;
    let mut id_constrained_seed_count = 0usize;
    let mut label_scan_seed_count = 0usize;
    let mut pivotable_pattern_count = 0usize;
    let mut fragile_pivot_count = 0usize;
    for op in &query.pipeline {
        if let CypherPipelineOp::Match(clause) = op {
            for pattern in &clause.patterns {
                let seed_binding_state =
                    explain_cypher_pattern_seed_binding_state(pattern, &available_vars);
                let seed_mode = explain_cypher_pattern_seed_binding_mode(pattern);
                if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                    pivotable_pattern_count += 1;
                    if delta <= 1 {
                        fragile_pivot_count += 1;
                    }
                }
                if seed_binding_state == "prebound" {
                    correlated_pattern_count += 1;
                    prebound_seed_count += 1;
                }
                if pattern
                    .relationships
                    .iter()
                    .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
                {
                    var_length_pattern_count += 1;
                }
                if pattern.path_variable.is_some() {
                    named_path_count += 1;
                }
                if pattern
                    .relationships
                    .iter()
                    .any(|rel| rel.direction == CypherRelDirection::Both)
                {
                    both_direction_pattern_count += 1;
                }
                if seed_mode == "id_constrained" {
                    id_constrained_seed_count += 1;
                }
                if seed_mode == "label_scan" {
                    label_scan_seed_count += 1;
                }
                available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
            }
        }
    }
    for clause in &query.matches {
        for pattern in &clause.patterns {
            let seed_binding_state =
                explain_cypher_pattern_seed_binding_state(pattern, &available_vars);
            let seed_mode = explain_cypher_pattern_seed_binding_mode(pattern);
            if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                pivotable_pattern_count += 1;
                if delta <= 1 {
                    fragile_pivot_count += 1;
                }
            }
            if seed_binding_state == "prebound" {
                correlated_pattern_count += 1;
                prebound_seed_count += 1;
            }
            if pattern
                .relationships
                .iter()
                .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
            {
                var_length_pattern_count += 1;
            }
            if pattern.path_variable.is_some() {
                named_path_count += 1;
            }
            if pattern
                .relationships
                .iter()
                .any(|rel| rel.direction == CypherRelDirection::Both)
            {
                both_direction_pattern_count += 1;
            }
            if seed_mode == "id_constrained" {
                id_constrained_seed_count += 1;
            }
            if seed_mode == "label_scan" {
                label_scan_seed_count += 1;
            }
            available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
        }
    }
    let return_fields = if query.returns.is_empty() {
        "none".to_owned()
    } else {
        query
            .returns
            .iter()
            .map(|projection| projection.field.name.clone())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "Graph Query Summary: pipeline_matches={}, top_level_matches={}, pipeline_patterns={}, top_level_patterns={}, optional_matches={}, correlated_patterns={}, prebound_seeds={}, id_constrained_seeds={}, label_scan_seeds={}, pivotable_patterns={}, fragile_pivots={}, var_length_patterns={}, named_paths={}, both_direction_patterns={}, returns={}, return_fields={}, order_by={}, distinct={}, creates={}, merges={}, sets={}, deletes={}, call_subqueries={}, foreachs={}, union={}",
        pipeline_match_count,
        query.matches.len(),
        pipeline_pattern_count,
        top_level_pattern_count,
        optional_match_count,
        correlated_pattern_count,
        prebound_seed_count,
        id_constrained_seed_count,
        label_scan_seed_count,
        pivotable_pattern_count,
        fragile_pivot_count,
        var_length_pattern_count,
        named_path_count,
        both_direction_pattern_count,
        query.returns.len(),
        return_fields,
        query.order_by.len(),
        query.distinct,
        query.creates.len(),
        query.merges.len(),
        query.sets.len(),
        query.deletes.len(),
        pipeline_call_count,
        pipeline_foreach_count,
        query.union.is_some(),
    )
}

fn explain_selectivity_ratio(numerator: Option<u64>, denominator: Option<u64>) -> String {
    match (numerator, denominator) {
        (Some(_), Some(0)) => "undefined".to_owned(),
        (Some(num), Some(den)) => format!("{:.3}", (num as f64) / (den as f64)),
        _ => "unknown".to_owned(),
    }
}

fn explain_query_runtime_strategy(query: &CypherQueryPlan) -> (&'static str, &'static str) {
    if !query.creates.is_empty()
        || !query.merges.is_empty()
        || !query.sets.is_empty()
        || !query.deletes.is_empty()
        || query.distinct
        || query.skip.is_some()
        || query.union.is_some()
    {
        return ("general_graph_runtime", "query_shape_not_fast_path_eligible");
    }

    let match_clause = match query.pipeline.as_slice() {
        [CypherPipelineOp::Match(match_clause)] => match_clause,
        [] if query.matches.len() == 1 => &query.matches[0],
        _ => return ("general_graph_runtime", "query_shape_not_fast_path_eligible"),
    };
    if match_clause.optional || match_clause.patterns.len() != 1 {
        return ("general_graph_runtime", "query_shape_not_fast_path_eligible");
    }

    let pattern = &match_clause.patterns[0];
    if pattern.path_function.is_some() || pattern.nodes.len() != 2 || pattern.relationships.len() != 1
    {
        return ("general_graph_runtime", "query_shape_not_fast_path_eligible");
    }

    let left = &pattern.nodes[0];
    let right = &pattern.nodes[1];
    let rel = &pattern.relationships[0];
    let Some(left_variable) = left.variable.as_deref() else {
        return ("general_graph_runtime", "query_shape_not_fast_path_eligible");
    };
    let Some(right_variable) = right.variable.as_deref() else {
        return ("general_graph_runtime", "query_shape_not_fast_path_eligible");
    };
    let return_name = query.returns.first().and_then(|projection| column_ref_name(&projection.expr));
    let expected_right = format!("{right_variable}.id");

    if left.table_id.is_some()
        && right.table_id.is_some()
        && rel.table_id.is_some()
        && rel.direction == CypherRelDirection::Outgoing
        && rel.variable.is_none()
        && rel.min_hops.is_none()
        && rel.max_hops.is_none()
        && rel.properties.is_empty()
        && !node_has_filter_constraints(right)
        && return_name == Some(expected_right.as_str())
        && extract_start_id_literal(left, match_clause.filter.as_ref(), left_variable).is_some()
        && ascending_order_by_matches_column(&query.order_by, &expected_right)
    {
        return ("fast_one_hop_id_lookup", "anchored_start_id_to_target_id");
    }

    let returns_left = return_name.is_some_and(|name| is_graph_id_ref(name, left_variable));
    let returns_right = return_name.is_some_and(|name| is_graph_id_ref(name, right_variable));
    if left.table_id.is_some()
        && right.table_id.is_some()
        && rel.table_id.is_some()
        && rel.variable.is_none()
        && rel.min_hops.is_none()
        && rel.max_hops.is_none()
        && rel.properties.is_empty()
        && ((extract_start_id_literal(left, match_clause.filter.as_ref(), left_variable).is_some()
            && returns_right
            && !node_has_filter_constraints(right))
            || (extract_start_id_literal(right, match_clause.filter.as_ref(), right_variable)
                .is_some()
                && returns_left
                && !node_has_filter_constraints(left)))
        && return_name.is_some_and(|name| ascending_order_by_matches_column(&query.order_by, name))
    {
        return (
            "fast_one_hop_endpoint_id_lookup",
            "anchored_endpoint_id_lookup",
        );
    }

    let expected_right = format!("{right_variable}.id");
    if query.order_by.is_empty()
        && query
            .limit
            .as_ref()
            .and_then(literal_i64)
            .is_some_and(|value| value > 0)
        && left.table_id.is_some()
        && right.table_id.is_some()
        && rel.table_id.is_some()
        && left.properties.is_empty()
        && right.properties.is_empty()
        && rel.direction == CypherRelDirection::Outgoing
        && rel.min_hops.is_none()
        && rel.max_hops.is_none()
        && rel.properties.is_empty()
        && return_name == Some(expected_right.as_str())
    {
        if let Some(rel_variable) = rel.variable.as_deref() {
            if match_clause.filter.as_ref().and_then(|filter| {
                exact_named_column_literal_gt(filter, &format!("{rel_variable}.weight"))
            }).is_some()
            {
                return (
                    "fast_unanchored_edge_filter_limit",
                    "unanchored_edge_weight_gt_limit",
                );
            }
        }
    }

    if query.order_by.is_empty()
        && query
            .limit
            .as_ref()
            .and_then(literal_i64)
            .is_some_and(|value| value > 0)
        && match_clause.filter.is_none()
        && left.table_id.is_some()
        && right.table_id.is_some()
        && rel.table_id.is_some()
        && left.properties.is_empty()
        && right.properties.is_empty()
        && rel.direction == CypherRelDirection::Outgoing
        && rel.min_hops.is_none()
        && rel.max_hops.is_none()
        && return_name == Some(expected_right.as_str())
        && rel.properties.len() == 1
        && rel.properties[0].key.eq_ignore_ascii_case("weight")
        && literal_value(&rel.properties[0].value).is_some()
    {
        return (
            "fast_unanchored_edge_eq_filter_limit",
            "unanchored_edge_weight_eq_limit",
        );
    }

    if query.order_by.is_empty()
        && query
            .limit
            .as_ref()
            .and_then(literal_i64)
            .is_some_and(|value| value > 0)
        && match_clause.filter.is_none()
        && pattern.path_function.is_none()
        && pattern.nodes.len() == 2
        && pattern.relationships.len() == 1
        && left.table_id.is_some()
        && right.table_id.is_some()
        && left.properties.is_empty()
        && right.properties.is_empty()
        && rel.table_id.is_some()
        && rel.direction == CypherRelDirection::Outgoing
        && rel.variable.is_none()
        && rel.min_hops.is_none()
        && rel.max_hops.is_none()
        && rel.properties.is_empty()
        && return_name
            .and_then(|name| name.strip_prefix(right_variable).and_then(|tail| tail.strip_prefix('.')))
            .is_some()
    {
        return (
            "fast_unanchored_one_hop_limit",
            "single_hop_projection_limit",
        );
    }

    ("general_graph_runtime", "query_shape_not_fast_path_eligible")
}

fn resolve_query_runtime_strategy(
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String, String) {
    let (fallback_strategy, fallback_reason) = explain_query_runtime_strategy(query);
    if let Some(values) = runtime_text {
        if let Some(strategy) = values.get("query_runtime_strategy") {
            let reason = values
                .get("query_runtime_reason")
                .cloned()
                .unwrap_or_else(|| fallback_reason.to_owned());
            return (strategy.clone(), reason, "observed".to_owned());
        }
    }
    (
        fallback_strategy.to_owned(),
        fallback_reason.to_owned(),
        "inferred".to_owned(),
    )
}

fn explain_elapsed_ms(elapsed_nanos: Option<u64>) -> String {
    elapsed_nanos
        .map(|nanos| format!("{:.3}", (nanos as f64) / 1_000_000.0))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn explain_estimate_error_ratio(estimated_rows: Option<u64>, actual_rows: Option<u64>) -> String {
    match (estimated_rows, actual_rows) {
        (Some(_), Some(0)) => "undefined".to_owned(),
        (Some(estimated), Some(actual)) => format!("{:.3}", (estimated as f64) / (actual as f64)),
        _ => "unknown".to_owned(),
    }
}

pub(in crate::executor) fn graph_estimate_warning_severity(
    estimated_rows: Option<u64>,
    actual_rows: Option<u64>,
) -> Option<&'static str> {
    let (estimated, actual) = match (estimated_rows, actual_rows) {
        (Some(estimated), Some(actual)) if actual > 0 => (estimated as f64, actual as f64),
        _ => return None,
    };
    let ratio = estimated / actual;
    if !(0.25..=4.0).contains(&ratio) {
        Some("high")
    } else if !(0.5..=2.0).contains(&ratio) {
        Some("medium")
    } else {
        None
    }
}

fn explain_graph_drift_summary_line(
    txn_id: TxnId,
    executor: &Executor,
    query: &CypherQueryPlan,
    actual_rows: &HashMap<String, u64>,
) -> (String, usize, usize) {
    let mut warning_count = 0usize;
    let mut high_warning_count = 0usize;
    let mut compared_patterns = 0usize;

    for (clause_index, op) in query.pipeline.iter().enumerate() {
        if let CypherPipelineOp::Match(clause) = op {
            for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                let estimated_rows =
                    executor.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern).estimated_rows;
                let actual_rows = actual_rows
                    .get(&graph_access_profile_key("PipelineMatch", clause_index, pattern_index))
                    .copied();
                if estimated_rows.is_some() && actual_rows.is_some() {
                    compared_patterns += 1;
                }
                match graph_estimate_warning_severity(estimated_rows, actual_rows) {
                    Some("high") => {
                        high_warning_count += 1;
                        warning_count += 1;
                    }
                    Some(_) => {
                        warning_count += 1;
                    }
                    None => {}
                }
            }
        }
    }

    for (clause_index, clause) in query.matches.iter().enumerate() {
        for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
            let estimated_rows =
                executor.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern).estimated_rows;
            let actual_rows = actual_rows
                .get(&graph_access_profile_key("Match", clause_index, pattern_index))
                .copied();
            if estimated_rows.is_some() && actual_rows.is_some() {
                compared_patterns += 1;
            }
            match graph_estimate_warning_severity(estimated_rows, actual_rows) {
                Some("high") => {
                    high_warning_count += 1;
                    warning_count += 1;
                }
                Some(_) => {
                    warning_count += 1;
                }
                None => {}
            }
        }
    }

    (
        format!(
            "Graph Drift Summary: compared_patterns={}, warnings={}, high_warnings={}, source=observed",
            compared_patterns, warning_count, high_warning_count
        ),
        warning_count,
        high_warning_count,
    )
}

pub(in crate::executor) fn explain_graph_drift_suggestion_line(
    warning_count: usize,
    high_warning_count: usize,
) -> Option<String> {
    if high_warning_count > 0 {
        Some(
            "Graph Suggestion: high estimate drift detected; inspect graph stats freshness, seed selectivity, and missing property indexes on seed or edge filters; source=observed"
                .to_owned(),
        )
    } else if warning_count > 0 {
        Some(
            "Graph Suggestion: moderate estimate drift detected; compare estimated vs actual rows on flagged patterns and review graph statistics coverage; source=observed"
                .to_owned(),
        )
    } else {
        None
    }
}

pub(in crate::executor) fn explain_graph_plan_hint_line(high_warning_count: usize) -> Option<String> {
    if high_warning_count > 0 {
        Some(
            "Graph Plan Hint: seed/pivot choice is likely unstable; prefer reviewing graph statistics and adding selective property indexes before tuning query shape"
                .to_owned(),
        )
    } else {
        None
    }
}

struct GraphPivotSummaryMetrics {
    pivotable_patterns: usize,
    fragile_pivots: usize,
    blocked_pivots: usize,
    selected_non_leftmost: usize,
    fragile_sites: Vec<String>,
}

fn graph_pivot_summary_metrics(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> GraphPivotSummaryMetrics {
    let mut pivotable_patterns = 0usize;
    let mut fragile_pivots = 0usize;
    let mut blocked_pivots = 0usize;
    let mut selected_non_leftmost = 0usize;
    let mut fragile_sites = Vec::new();

    for (clause_index, op) in query.pipeline.iter().enumerate() {
        if let CypherPipelineOp::Match(clause) = op {
            for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                if pattern.nodes.len() <= 1 {
                    continue;
                }
                if pattern
                    .relationships
                    .iter()
                    .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
                {
                    blocked_pivots += 1;
                    continue;
                }
                pivotable_patterns += 1;
                if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                    if delta <= 1 {
                        fragile_pivots += 1;
                        fragile_sites.push(format!("PipelineMatch{}.{}", clause_index, pattern_index));
                    }
                }
                let (pattern_runtime_strategy, _, _, _) = resolve_pattern_runtime_strategy(
                    "PipelineMatch",
                    clause_index,
                    pattern_index,
                    runtime_text,
                );
                if pattern_runtime_strategy == "pivoted_node_seed"
                    || (pattern_runtime_strategy == "unknown"
                        && explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern)
                            .is_some())
                {
                    selected_non_leftmost += 1;
                }
            }
        }
    }

    for (clause_index, clause) in query.matches.iter().enumerate() {
        for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
            if pattern.nodes.len() <= 1 {
                continue;
            }
            if pattern
                .relationships
                .iter()
                .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
            {
                blocked_pivots += 1;
                continue;
            }
            pivotable_patterns += 1;
            if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                if delta <= 1 {
                    fragile_pivots += 1;
                    fragile_sites.push(format!("Match{}.{}", clause_index, pattern_index));
                }
            }
            let (pattern_runtime_strategy, _, _, _) = resolve_pattern_runtime_strategy(
                "Match",
                clause_index,
                pattern_index,
                runtime_text,
            );
            if pattern_runtime_strategy == "pivoted_node_seed"
                || (pattern_runtime_strategy == "unknown"
                    && explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern)
                        .is_some())
            {
                selected_non_leftmost += 1;
            }
        }
    }

    GraphPivotSummaryMetrics {
        pivotable_patterns,
        fragile_pivots,
        blocked_pivots,
        selected_non_leftmost,
        fragile_sites,
    }
}

struct GraphPivotDriverMetrics {
    cbo_pivoted: usize,
    heuristic_pivoted: usize,
}

fn graph_pivot_driver_metrics(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> GraphPivotDriverMetrics {
    let mut cbo_pivoted = 0usize;
    let mut heuristic_pivoted = 0usize;

    for (clause_index, clause) in query.pipeline.iter().filter_map(|op| match op {
        CypherPipelineOp::Match(clause) => Some(clause),
        _ => None,
    }).enumerate() {
        for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
            if explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern).is_none() {
                continue;
            }
            let (pivot_driver, _) = resolve_pattern_pivot_driver(
                executor,
                txn_id,
                pattern,
                "PipelineMatch",
                clause_index,
                pattern_index,
                runtime_text,
            );
            match pivot_driver.as_str() {
                "cbo" => cbo_pivoted += 1,
                "heuristic" => heuristic_pivoted += 1,
                _ => {}
            }
        }
    }

    for (clause_index, clause) in query.matches.iter().enumerate() {
        for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
            if explain_cypher_pattern_runtime_pivot_index(executor, txn_id, pattern).is_none() {
                continue;
            }
            let (pivot_driver, _) = resolve_pattern_pivot_driver(
                executor,
                txn_id,
                pattern,
                "Match",
                clause_index,
                pattern_index,
                runtime_text,
            );
            match pivot_driver.as_str() {
                "cbo" => cbo_pivoted += 1,
                "heuristic" => heuristic_pivoted += 1,
                _ => {}
            }
        }
    }

    GraphPivotDriverMetrics {
        cbo_pivoted,
        heuristic_pivoted,
    }
}

struct GraphPivotMetricSources {
    selected_non_leftmost_source: &'static str,
    pivot_driver_metrics_source: &'static str,
}

fn graph_pivot_metric_sources(
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> GraphPivotMetricSources {
    let Some(runtime_text) = runtime_text else {
        return GraphPivotMetricSources {
            selected_non_leftmost_source: "inferred",
            pivot_driver_metrics_source: "inferred",
        };
    };

    let mut selected_non_leftmost_source = "inferred";
    let mut pivot_driver_metrics_source = "inferred";

    let mut consider_clause = |clause_label: &str, clause_index: usize, clause: &CypherMatchClause| {
        for pattern_index in 0..clause.patterns.len() {
            if runtime_text.contains_key(&graph_access_pattern_runtime_strategy_key(
                clause_label,
                clause_index,
                pattern_index,
            )) {
                selected_non_leftmost_source = "observed";
            }
            if runtime_text.contains_key(&graph_access_pattern_pivot_driver_key(
                clause_label,
                clause_index,
                pattern_index,
            )) {
                pivot_driver_metrics_source = "observed";
            }
        }
    };

    for (clause_index, op) in query.pipeline.iter().enumerate() {
        if let CypherPipelineOp::Match(clause) = op {
            consider_clause("PipelineMatch", clause_index, clause);
        }
    }

    for (clause_index, clause) in query.matches.iter().enumerate() {
        consider_clause("Match", clause_index, clause);
    }

    GraphPivotMetricSources {
        selected_non_leftmost_source,
        pivot_driver_metrics_source,
    }
}

struct GraphActualMetricSources {
    drift_metrics_source: &'static str,
    join_risk_metrics_source: &'static str,
}

fn graph_actual_metric_sources(
    actual_rows: Option<&HashMap<String, u64>>,
) -> GraphActualMetricSources {
    if actual_rows.is_some() {
        GraphActualMetricSources {
            drift_metrics_source: "observed",
            join_risk_metrics_source: "observed",
        }
    } else {
        GraphActualMetricSources {
            drift_metrics_source: "unavailable",
            join_risk_metrics_source: "unavailable",
        }
    }
}

struct GraphAccessSourceMetrics {
    total_patterns: usize,
    row_store_source: usize,
    traversal_store_source: usize,
    projection_store_source: usize,
    hybrid_source: usize,
    row_fallback_patterns: usize,
    row_store_traversal_patterns: usize,
}

fn graph_access_source_metrics(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
) -> GraphAccessSourceMetrics {
    let context = ExecutionContext {
        txn_id,
        ..ExecutionContext::default()
    };
    let hints = executor.describe_cypher_query_graph_plans(&context, query);
    let mut row_store_source = 0usize;
    let mut traversal_store_source = 0usize;
    let mut projection_store_source = 0usize;
    let mut hybrid_source = 0usize;
    let mut row_fallback = 0usize;
    let mut row_store_traversal_patterns = 0usize;

    for hint in &hints {
        match hint.plan.source {
            Some(HybridGraphSource::RowStore) => {
                row_store_source += 1;
                if hint
                    .plan
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("relationship pattern"))
                {
                    row_store_traversal_patterns += 1;
                }
            }
            Some(HybridGraphSource::TraversalStore) => traversal_store_source += 1,
            Some(HybridGraphSource::ProjectionStore) => projection_store_source += 1,
            Some(HybridGraphSource::Hybrid) => hybrid_source += 1,
            Some(HybridGraphSource::VectorIndex) | None => {}
        }
        if hint.plan.fallback_source == Some(HybridGraphSource::RowStore) {
            row_fallback += 1;
        }
    }

    GraphAccessSourceMetrics {
        total_patterns: hints.len(),
        row_store_source,
        traversal_store_source,
        projection_store_source,
        hybrid_source,
        row_fallback_patterns: row_fallback,
        row_store_traversal_patterns,
    }
}

struct GraphProcedureSourceMetrics {
    total_procedures: usize,
    projection_store_source: usize,
    row_fallback_procedures: usize,
    weighted_projection_procedures: usize,
}

fn graph_procedure_source_metrics(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
) -> GraphProcedureSourceMetrics {
    let hints = executor.describe_cypher_query_graph_procedure_plans(txn_id, query);
    let mut projection_store_source = 0usize;
    let mut row_fallback = 0usize;
    let mut weighted_projection = 0usize;

    for hint in &hints {
        if hint.plan.source == Some(HybridGraphSource::ProjectionStore) {
            projection_store_source += 1;
        }
        if hint.plan.fallback_source == Some(HybridGraphSource::RowStore) {
            row_fallback += 1;
        }
        if hint.weighted {
            weighted_projection += 1;
        }
    }

    GraphProcedureSourceMetrics {
        total_procedures: hints.len(),
        projection_store_source,
        row_fallback_procedures: row_fallback,
        weighted_projection_procedures: weighted_projection,
    }
}

struct GraphFragilePivotBreakdown {
    exact_ties: usize,
    near_ties: usize,
    prebound_fragile: usize,
    label_scan_fragile: usize,
}

fn graph_fragile_pivot_breakdown(query: &CypherQueryPlan) -> GraphFragilePivotBreakdown {
    let mut exact_ties = 0usize;
    let mut near_ties = 0usize;
    let mut prebound_fragile = 0usize;
    let mut label_scan_fragile = 0usize;
    let mut available_vars = HashSet::new();

    for op in &query.pipeline {
        if let CypherPipelineOp::Match(clause) = op {
            for pattern in &clause.patterns {
                if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                    if delta <= 1 {
                        if delta == 0 {
                            exact_ties += 1;
                        } else {
                            near_ties += 1;
                        }
                        if explain_cypher_pattern_seed_binding_state(pattern, &available_vars)
                            == "prebound"
                        {
                            prebound_fragile += 1;
                        }
                        if explain_cypher_pattern_seed_binding_mode(pattern) == "label_scan" {
                            label_scan_fragile += 1;
                        }
                    }
                }
                available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
            }
        }
    }

    for clause in &query.matches {
        for pattern in &clause.patterns {
            if let Some(delta) = cypher_pattern_pivot_delta(pattern) {
                if delta <= 1 {
                    if delta == 0 {
                        exact_ties += 1;
                    } else {
                        near_ties += 1;
                    }
                    if explain_cypher_pattern_seed_binding_state(pattern, &available_vars)
                        == "prebound"
                    {
                        prebound_fragile += 1;
                    }
                    if explain_cypher_pattern_seed_binding_mode(pattern) == "label_scan" {
                        label_scan_fragile += 1;
                    }
                }
            }
            available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
        }
    }

    GraphFragilePivotBreakdown {
        exact_ties,
        near_ties,
        prebound_fragile,
        label_scan_fragile,
    }
}

struct GraphJoinSummaryMetrics {
    multi_pattern_clauses: usize,
    correlated_clauses: usize,
    shared_anchor_clauses: usize,
    max_correlated_vars_per_pattern: usize,
    correlated_shared_anchor: usize,
    correlated_non_shared: usize,
    shared_anchor_uncorrelated: usize,
    independent_multi_scan: usize,
    correlated_sites: Vec<String>,
}

fn graph_join_summary_metrics(query: &CypherQueryPlan) -> GraphJoinSummaryMetrics {
    let mut multi_pattern_clauses = 0usize;
    let mut correlated_clauses = 0usize;
    let mut shared_anchor_clauses = 0usize;
    let mut max_correlated_vars_per_pattern = 0usize;
    let mut correlated_shared_anchor = 0usize;
    let mut correlated_non_shared = 0usize;
    let mut shared_anchor_uncorrelated = 0usize;
    let mut independent_multi_scan = 0usize;
    let mut correlated_sites = Vec::new();
    let mut available_vars = HashSet::new();

    let mut scan_clause = |clause_kind: &str, clause_index: usize, clause: &CypherMatchClause| {
        if clause.patterns.len() > 1 {
            multi_pattern_clauses += 1;
        }
        let mut clause_is_correlated_flag = false;
        let mut clause_shared_anchor = clause.patterns.len() > 1;
        let expected_anchor = clause
            .patterns
            .first()
            .and_then(|pattern| pattern.nodes.first())
            .and_then(|node| node.variable.as_deref())
            .map(str::to_owned);

        for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
            let correlated = explain_cypher_pattern_correlated_vars(pattern, &available_vars);
            let correlated_count = if correlated == "none" {
                0
            } else {
                correlated.split(',').count()
            };
            max_correlated_vars_per_pattern =
                max_correlated_vars_per_pattern.max(correlated_count);
            if pattern_index > 0 && correlated_count > 0 {
                clause_is_correlated_flag = true;
                correlated_sites.push(format!("{clause_kind}{clause_index}.{pattern_index}"));
            }
            if clause_shared_anchor {
                let seed_var = pattern
                    .nodes
                    .first()
                    .and_then(|node| node.variable.as_deref())
                    .map(str::to_owned);
                if pattern_index > 0 && (seed_var.is_none() || seed_var != expected_anchor) {
                    clause_shared_anchor = false;
                }
            }
            available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
        }

        if clause_is_correlated_flag {
            correlated_clauses += 1;
        }
        if clause_shared_anchor {
            shared_anchor_clauses += 1;
        }
        if clause.patterns.len() > 1 {
            match (clause_is_correlated_flag, clause_shared_anchor) {
                (true, true) => correlated_shared_anchor += 1,
                (true, false) => correlated_non_shared += 1,
                (false, true) => shared_anchor_uncorrelated += 1,
                (false, false) => independent_multi_scan += 1,
            }
        }
    };

    for (clause_index, op) in query.pipeline.iter().enumerate() {
        if let CypherPipelineOp::Match(clause) = op {
            scan_clause("PipelineMatch", clause_index, clause);
        }
    }
    for (clause_index, clause) in query.matches.iter().enumerate() {
        scan_clause("Match", clause_index, clause);
    }

    GraphJoinSummaryMetrics {
        multi_pattern_clauses,
        correlated_clauses,
        shared_anchor_clauses,
        max_correlated_vars_per_pattern,
        correlated_shared_anchor,
        correlated_non_shared,
        shared_anchor_uncorrelated,
        independent_multi_scan,
        correlated_sites,
    }
}

fn explain_graph_pivot_summary_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> Option<String> {
    let summary_metrics = graph_pivot_summary_metrics(executor, txn_id, query, runtime_text);
    let driver_metrics = graph_pivot_driver_metrics(executor, txn_id, query, runtime_text);
    let metric_sources = graph_pivot_metric_sources(query, runtime_text);

    if summary_metrics.pivotable_patterns == 0 && summary_metrics.blocked_pivots == 0 {
        return None;
    }

    Some(format!(
        "Graph Pivot Summary: pivotable_patterns={}, fragile_pivots={}, blocked_pivots={}, selected_non_leftmost={}, selected_non_leftmost_source={}, cbo_pivoted={}, heuristic_pivoted={}, pivot_driver_metrics_source={}, fragile_sites={}",
        summary_metrics.pivotable_patterns,
        summary_metrics.fragile_pivots,
        summary_metrics.blocked_pivots,
        summary_metrics.selected_non_leftmost,
        metric_sources.selected_non_leftmost_source,
        driver_metrics.cbo_pivoted,
        driver_metrics.heuristic_pivoted,
        metric_sources.pivot_driver_metrics_source,
        if summary_metrics.fragile_sites.is_empty() {
            "none".to_owned()
        } else {
            summary_metrics.fragile_sites.join(",")
        }
    ))
}

fn explain_graph_access_summary_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
) -> Option<String> {
    let metrics = graph_access_source_metrics(executor, txn_id, query);
    if metrics.total_patterns == 0 {
        return None;
    }
    Some(format!(
        "Graph Access Summary: total_patterns={}, row_store_source={}, traversal_store_source={}, projection_store_source={}, hybrid_source={}, row_fallback_patterns={}, row_store_traversal_patterns={}, source=inferred",
        metrics.total_patterns,
        metrics.row_store_source,
        metrics.traversal_store_source,
        metrics.projection_store_source,
        metrics.hybrid_source,
        metrics.row_fallback_patterns,
        metrics.row_store_traversal_patterns
    ))
}

fn explain_graph_access_warning_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
) -> Option<String> {
    let metrics = graph_access_source_metrics(executor, txn_id, query);
    if metrics.row_fallback_patterns == 0 && metrics.row_store_traversal_patterns == 0 {
        return None;
    }
    Some(format!(
        "Graph Access Warning: {} relationship patterns are row-store only and {} patterns still keep a row-store fallback; inspect native adjacency coverage before trusting graph latency; source=inferred",
        metrics.row_store_traversal_patterns, metrics.row_fallback_patterns
    ))
}

fn explain_graph_procedure_summary_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
) -> Option<String> {
    let metrics = graph_procedure_source_metrics(executor, txn_id, query);
    if metrics.total_procedures == 0 {
        return None;
    }
    Some(format!(
        "Graph Procedure Summary: total_procedures={}, projection_store_source={}, row_fallback_procedures={}, weighted_projection={}, source=inferred",
        metrics.total_procedures,
        metrics.projection_store_source,
        metrics.row_fallback_procedures,
        metrics.weighted_projection_procedures
    ))
}

fn explain_graph_pivot_hint_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> Option<String> {
    let source = if runtime_text.is_some() {
        "observed"
    } else {
        "inferred"
    };
    let metrics = graph_pivot_summary_metrics(executor, txn_id, query, runtime_text);
    if metrics.pivotable_patterns == 0 || metrics.fragile_pivots == 0 {
        return None;
    }
    let breakdown = graph_fragile_pivot_breakdown(query);
    Some(format!(
        "Graph Pivot Hint: {} of {} pivotable patterns are fragile; exact_ties={}, near_ties={}, prebound_fragile={}, label_scan_fragile={}; review seed selectivity and early filters around {} before trusting the current left-to-right shape (selected_non_leftmost={}); source={}",
        metrics.fragile_pivots,
        metrics.pivotable_patterns,
        breakdown.exact_ties,
        breakdown.near_ties,
        breakdown.prebound_fragile,
        breakdown.label_scan_fragile,
        metrics.fragile_sites.join(","),
        metrics.selected_non_leftmost,
        source
    ))
}

fn explain_graph_pivot_note_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> Option<String> {
    let source = if runtime_text.is_some() {
        "observed"
    } else {
        "inferred"
    };
    let metrics = graph_pivot_summary_metrics(executor, txn_id, query, runtime_text);
    if metrics.pivotable_patterns == 0
        || metrics.selected_non_leftmost == 0
        || metrics.fragile_pivots > 0
    {
        return None;
    }
    Some(format!(
        "Graph Pivot Note: planner selected a non-leftmost seed in {} of {} pivotable patterns; current query shape already depends on local selectivity reordering; source={}",
        metrics.selected_non_leftmost,
        metrics.pivotable_patterns,
        source
    ))
}

fn explain_graph_planner_warning_line(
    executor: &Executor,
    txn_id: TxnId,
    query: &CypherQueryPlan,
    runtime_text: Option<&HashMap<String, String>>,
) -> Option<String> {
    let source = if runtime_text.is_some() {
        "observed"
    } else {
        "inferred"
    };
    let metrics = graph_pivot_summary_metrics(executor, txn_id, query, runtime_text);
    if metrics.fragile_pivots > 0 {
        return Some(format!(
            "Graph Planner Warning: pivot stability is weak on {} of {} pivotable patterns; seed choice may change materially as data distribution shifts; source={}",
            metrics.fragile_pivots, metrics.pivotable_patterns, source
        ));
    }
    if metrics.blocked_pivots > 0 && metrics.selected_non_leftmost > 0 {
        return Some(format!(
            "Graph Planner Warning: plan mixes blocked var-length pivots ({}) with selective non-leftmost seeds ({}); inspect whether path-heavy clauses dominate the overall shape; source={}",
            metrics.blocked_pivots, metrics.selected_non_leftmost, source
        ));
    }
    None
}

fn explain_graph_join_summary_line(query: &CypherQueryPlan) -> Option<String> {
    let (
        multi_pattern_clauses,
        correlated_clauses,
        shared_anchor_clauses,
        max_correlated_vars_per_pattern,
        correlated_shared_anchor,
        correlated_non_shared,
        shared_anchor_uncorrelated,
        independent_multi_scan,
        correlated_sites,
    ) = {
        let metrics = graph_join_summary_metrics(query);
        (
            metrics.multi_pattern_clauses,
            metrics.correlated_clauses,
            metrics.shared_anchor_clauses,
            metrics.max_correlated_vars_per_pattern,
            metrics.correlated_shared_anchor,
            metrics.correlated_non_shared,
            metrics.shared_anchor_uncorrelated,
            metrics.independent_multi_scan,
            metrics.correlated_sites,
        )
    };
    if multi_pattern_clauses == 0 {
        return None;
    }
    Some(format!(
        "Graph Join Summary: multi_pattern_clauses={}, correlated_clauses={}, shared_anchor_clauses={}, max_correlated_vars_per_pattern={}, correlated_shared_anchor={}, correlated_non_shared={}, shared_anchor_uncorrelated={}, independent_multi_scan={}, correlated_sites={}",
        multi_pattern_clauses,
        correlated_clauses,
        shared_anchor_clauses,
        max_correlated_vars_per_pattern,
        correlated_shared_anchor,
        correlated_non_shared,
        shared_anchor_uncorrelated,
        independent_multi_scan,
        if correlated_sites.is_empty() {
            "none".to_owned()
        } else {
            correlated_sites.join(",")
        }
    ))
}

fn explain_graph_join_hint_line(query: &CypherQueryPlan) -> Option<String> {
    let source = "inferred";
    let (
        multi_pattern_clauses,
        correlated_clauses,
        shared_anchor_clauses,
        max_correlated_vars_per_pattern,
        correlated_shared_anchor,
        correlated_non_shared,
        shared_anchor_uncorrelated,
        independent_multi_scan,
        correlated_sites,
    ) = {
        let metrics = graph_join_summary_metrics(query);
        (
            metrics.multi_pattern_clauses,
            metrics.correlated_clauses,
            metrics.shared_anchor_clauses,
            metrics.max_correlated_vars_per_pattern,
            metrics.correlated_shared_anchor,
            metrics.correlated_non_shared,
            metrics.shared_anchor_uncorrelated,
            metrics.independent_multi_scan,
            metrics.correlated_sites,
        )
    };
    if correlated_clauses > 0 {
        if correlated_non_shared > 0 {
            return Some(format!(
                "Graph Join Hint: {} of {} multi-pattern clauses are correlated; correlated_non_shared={}, shared_anchor_clauses={}, max_correlated_vars_per_pattern={}; inspect fanout and variable reuse around {} before changing clause order; source={}",
                correlated_clauses,
                multi_pattern_clauses,
                correlated_non_shared,
                shared_anchor_clauses,
                max_correlated_vars_per_pattern,
                correlated_sites.join(","),
                source
            ));
        }
        return Some(format!(
            "Graph Join Hint: {} of {} multi-pattern clauses are correlated; correlated_shared_anchor={}, shared_anchor_clauses={}, max_correlated_vars_per_pattern={}; inspect fanout around {} before changing clause order; source={}",
            correlated_clauses,
            multi_pattern_clauses,
            correlated_shared_anchor,
            shared_anchor_clauses,
            max_correlated_vars_per_pattern,
            correlated_sites.join(","),
            source
        ));
    }
    if shared_anchor_uncorrelated > 0 {
        return Some(format!(
            "Graph Join Hint: {} of {} multi-pattern clauses share an anchor without correlation; shared_anchor_uncorrelated={}; watch star fanout before widening projections; source={}",
            shared_anchor_uncorrelated,
            multi_pattern_clauses,
            shared_anchor_uncorrelated,
            source
        ));
    }
    if independent_multi_scan > 0 {
        return Some(format!(
            "Graph Join Hint: {} of {} multi-pattern clauses are independent multi-scans; confirm this is intentional and not a missed join predicate; source={}",
            independent_multi_scan,
            multi_pattern_clauses,
            source
        ));
    }
    None
}

fn clause_is_correlated(
    clause: &CypherMatchClause,
    available_vars: &HashSet<String>,
) -> bool {
    let mut clause_available_vars = available_vars.clone();
    for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
        if pattern_index > 0
            && explain_cypher_pattern_correlated_vars(pattern, &clause_available_vars) != "none"
        {
            return true;
        }
        clause_available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
    }
    false
}

fn clause_is_shared_anchor(clause: &CypherMatchClause) -> bool {
    if clause.patterns.len() <= 1 {
        return false;
    }
    let expected_anchor = clause
        .patterns
        .first()
        .and_then(|pattern| pattern.nodes.first())
        .and_then(|node| node.variable.as_deref())
        .map(str::to_owned);
    clause.patterns.iter().enumerate().all(|(pattern_index, pattern)| {
        if pattern_index == 0 {
            true
        } else {
            pattern
                .nodes
                .first()
                .and_then(|node| node.variable.as_deref())
                .map(str::to_owned)
                == expected_anchor
        }
    })
}

fn explain_is_single_hop_star_pattern(
    pattern: &CypherPattern,
    anchor: &CypherNodePattern,
) -> bool {
    pattern.path_function.is_none()
        && pattern.path_variable.is_none()
        && pattern.nodes.len() == 2
        && pattern.relationships.len() == 1
        && pattern.nodes.first() == Some(anchor)
}

fn clause_runtime_strategy(clause: &CypherMatchClause) -> &'static str {
    let Some(anchor) = clause.patterns.first().and_then(|pattern| pattern.nodes.first()) else {
        return "pattern_by_pattern";
    };
    if !clause.optional
        && clause.patterns.len() >= 2
        && clause
            .patterns
            .iter()
            .all(|pattern| explain_is_single_hop_star_pattern(pattern, anchor))
    {
        "shared_anchor_star"
    } else {
        "pattern_by_pattern"
    }
}

fn clause_runtime_strategy_reason(clause: &CypherMatchClause) -> &'static str {
    let Some(anchor) = clause.patterns.first().and_then(|pattern| pattern.nodes.first()) else {
        return "empty_clause";
    };
    if !clause.optional
        && clause.patterns.len() >= 2
        && clause
            .patterns
            .iter()
            .all(|pattern| explain_is_single_hop_star_pattern(pattern, anchor))
    {
        return "all_patterns_single_hop_same_anchor";
    }
    if clause.optional {
        "optional_clause"
    } else if clause.patterns.len() < 2 {
        "single_pattern_clause"
    } else if clause_is_shared_anchor(clause) {
        "shared_anchor_non_single_hop_or_rewritten"
    } else {
        "general_multi_pattern_clause"
    }
}

fn clause_runtime_strategy_blocker(clause: &CypherMatchClause) -> Option<&'static str> {
    if clause_runtime_strategy(clause) == "shared_anchor_star" {
        return None;
    }
    if clause.optional {
        return Some("optional_clause");
    }
    if clause.patterns.len() < 2 {
        return Some("single_pattern_clause");
    }
    let anchor = clause.patterns.first().and_then(|pattern| pattern.nodes.first())?;
    if !clause_is_shared_anchor(clause) {
        return Some("anchor_not_shared");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| pattern.path_function.is_some() || pattern.path_variable.is_some())
    {
        return Some("path_binding_or_function_present");
    }
    if clause.patterns.iter().any(|pattern| pattern.nodes.len() != 2) {
        return Some("non_two_node_pattern_present");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| pattern.relationships.len() != 1)
    {
        return Some("non_single_relationship_pattern_present");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| !explain_is_single_hop_star_pattern(pattern, anchor))
    {
        return Some("pattern_not_single_hop_from_anchor");
    }
    Some("unknown")
}

fn clause_runtime_strategy_source() -> &'static str {
    "inferred"
}

fn resolve_pattern_pivot_driver(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String) {
    if let Some(values) = runtime_text {
        if let Some(driver) = values.get(&graph_access_pattern_pivot_driver_key(
            clause_label,
            clause_index,
            pattern_index,
        )) {
            return (driver.clone(), "observed".to_owned());
        }
    }
    (
        explain_cypher_pattern_pivot_driver(executor, txn_id, pattern),
        "inferred".to_owned(),
    )
}

fn resolve_pattern_pivot_reason(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String) {
    if let Some(values) = runtime_text {
        if let Some(reason) = values.get(&graph_access_pattern_pivot_reason_key(
            clause_label,
            clause_index,
            pattern_index,
        )) {
            return (reason.clone(), "observed".to_owned());
        }
    }
    (
        explain_cypher_pattern_pivot_reason(executor, txn_id, pattern),
        "inferred".to_owned(),
    )
}

fn resolve_pattern_pivot_decision(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String) {
    if let Some(values) = runtime_text {
        if let Some(decision) = values.get(&graph_access_pattern_pivot_decision_key(
            clause_label,
            clause_index,
            pattern_index,
        )) {
            return (decision.clone(), "observed".to_owned());
        }
    }
    (
        explain_cypher_pattern_pivot_decision(executor, txn_id, pattern),
        "inferred".to_owned(),
    )
}

fn resolve_pattern_runtime_strategy(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String, String, String) {
    if let Some(values) = runtime_text {
        if let Some(strategy) = values.get(&graph_access_pattern_runtime_strategy_key(
            clause_label,
            clause_index,
            pattern_index,
        ))
        {
            let reason = values
                .get(&graph_access_pattern_runtime_reason_key(
                    clause_label,
                    clause_index,
                    pattern_index,
                ))
                .cloned()
                .unwrap_or_else(|| "unknown".to_owned());
            return (
                strategy.clone(),
                "observed".to_owned(),
                reason,
                "observed".to_owned(),
            );
        }
    }
    (
        "unknown".to_owned(),
        "inferred".to_owned(),
        "unknown".to_owned(),
        "inferred".to_owned(),
    )
}

fn resolve_clause_runtime_strategy(
    clause_label: &str,
    clause_index: usize,
    clause: &CypherMatchClause,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String) {
    let inferred = clause_runtime_strategy(clause).to_owned();
    let observed = runtime_text
        .and_then(|values| values.get(&graph_access_clause_runtime_strategy_key(clause_label, clause_index)))
        .cloned();
    if let Some(observed) = observed {
        (observed, "observed".to_owned())
    } else {
        (inferred, clause_runtime_strategy_source().to_owned())
    }
}

fn resolve_clause_runtime_reason(
    clause_label: &str,
    clause_index: usize,
    clause: &CypherMatchClause,
    runtime_text: Option<&HashMap<String, String>>,
) -> (String, String) {
    if let Some(values) = runtime_text {
        if let Some(reason) =
            values.get(&graph_access_clause_runtime_reason_key(clause_label, clause_index))
        {
            return (reason.clone(), "observed".to_owned());
        }
    }
    (
        clause_runtime_strategy_reason(clause).to_owned(),
        "inferred".to_owned(),
    )
}

fn resolve_clause_runtime_blocker(
    clause_label: &str,
    clause_index: usize,
    clause: &CypherMatchClause,
    runtime_text: Option<&HashMap<String, String>>,
) -> (Option<String>, String) {
    if let Some(values) = runtime_text {
        if let Some(blocker) =
            values.get(&graph_access_clause_runtime_blocker_key(clause_label, clause_index))
        {
            return (Some(blocker.clone()), "observed".to_owned());
        }
        if values.contains_key(&graph_access_clause_runtime_strategy_key(clause_label, clause_index))
        {
            return (None, "observed".to_owned());
        }
    }
    (
        clause_runtime_strategy_blocker(clause).map(str::to_owned),
        "inferred".to_owned(),
    )
}

struct PatternRuntimeExplain {
    strategy: String,
    strategy_source: String,
    reason: String,
    reason_source: String,
    pivot_driver: String,
    pivot_driver_source: String,
    pivot_reason: String,
    pivot_reason_source: String,
    pivot_decision: String,
    pivot_decision_source: String,
}

fn explain_pattern_runtime(
    executor: &Executor,
    txn_id: TxnId,
    pattern: &CypherPattern,
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
    runtime_text: Option<&HashMap<String, String>>,
) -> PatternRuntimeExplain {
    let (strategy, strategy_source, reason, reason_source) =
        resolve_pattern_runtime_strategy(clause_label, clause_index, pattern_index, runtime_text);
    let (pivot_driver, pivot_driver_source) = resolve_pattern_pivot_driver(
        executor,
        txn_id,
        pattern,
        clause_label,
        clause_index,
        pattern_index,
        runtime_text,
    );
    let (pivot_reason, pivot_reason_source) = resolve_pattern_pivot_reason(
        executor,
        txn_id,
        pattern,
        clause_label,
        clause_index,
        pattern_index,
        runtime_text,
    );
    let (pivot_decision, pivot_decision_source) = resolve_pattern_pivot_decision(
        executor,
        txn_id,
        pattern,
        clause_label,
        clause_index,
        pattern_index,
        runtime_text,
    );
    PatternRuntimeExplain {
        strategy,
        strategy_source,
        reason,
        reason_source,
        pivot_driver,
        pivot_driver_source,
        pivot_reason,
        pivot_reason_source,
        pivot_decision,
        pivot_decision_source,
    }
}

struct PatternMetricExplain {
    estimated_rows_value: Option<u64>,
    estimated_rows_text: String,
    actual_rows_value: Option<u64>,
    actual_rows_text: String,
    estimate_error_ratio_text: String,
    estimate_error_ratio_value: Option<f64>,
    estimated_selectivity_text: String,
    estimated_selectivity_value: Option<f64>,
    actual_selectivity_text: String,
    actual_selectivity_value: Option<f64>,
    actual_time_ms_text: String,
    actual_time_ms_value: Option<f64>,
}

fn explain_pattern_metrics(
    estimated_rows_value: Option<u64>,
    actual_rows_value: Option<u64>,
    clause_input_rows: Option<u64>,
    actual_time_nanos: Option<u64>,
) -> PatternMetricExplain {
    let estimated_selectivity_value = ratio_value(estimated_rows_value, clause_input_rows);
    let actual_selectivity_value = ratio_value(actual_rows_value, clause_input_rows);
    let estimate_error_ratio_value =
        estimate_error_ratio_value(estimated_rows_value, actual_rows_value);
    let actual_time_ms_value = elapsed_ms_value(actual_time_nanos);
    PatternMetricExplain {
        estimated_rows_value,
        estimated_rows_text: estimated_rows_value
            .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
        actual_rows_value,
        actual_rows_text: actual_rows_value
            .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
        estimate_error_ratio_text: explain_estimate_error_ratio(
            estimated_rows_value,
            actual_rows_value,
        ),
        estimate_error_ratio_value,
        estimated_selectivity_text: explain_selectivity_ratio(
            estimated_rows_value,
            clause_input_rows,
        ),
        estimated_selectivity_value,
        actual_selectivity_text: explain_selectivity_ratio(
            actual_rows_value,
            clause_input_rows,
        ),
        actual_selectivity_value,
        actual_time_ms_text: explain_elapsed_ms(actual_time_nanos),
        actual_time_ms_value,
    }
}

struct ClauseRuntimeExplain {
    strategy: String,
    strategy_source: String,
    reason: String,
    reason_source: String,
    blocker: Option<String>,
    blocker_source: String,
}

fn explain_clause_runtime(
    clause_label: &str,
    clause_index: usize,
    clause: &CypherMatchClause,
    runtime_text: Option<&HashMap<String, String>>,
) -> ClauseRuntimeExplain {
    let (strategy, strategy_source) =
        resolve_clause_runtime_strategy(clause_label, clause_index, clause, runtime_text);
    let (reason, reason_source) =
        resolve_clause_runtime_reason(clause_label, clause_index, clause, runtime_text);
    let (blocker, blocker_source) =
        resolve_clause_runtime_blocker(clause_label, clause_index, clause, runtime_text);
    ClauseRuntimeExplain {
        strategy,
        strategy_source,
        reason,
        reason_source,
        blocker,
        blocker_source,
    }
}

fn join_risk_source_from_basis(basis: &str) -> &'static str {
    match basis {
        "actual" => "observed",
        "estimated" => "inferred",
        _ => "unavailable",
    }
}

fn graph_join_fanout_severity(input_rows: Option<u64>, output_rows: Option<u64>) -> Option<&'static str> {
    let (input_rows, output_rows) = match (input_rows, output_rows) {
        (Some(input_rows), Some(output_rows)) if input_rows > 0 => {
            (input_rows as f64, output_rows as f64)
        }
        _ => return None,
    };
    let ratio = output_rows / input_rows;
    if ratio > 4.0 {
        Some("high")
    } else if ratio > 2.0 {
        Some("medium")
    } else {
        None
    }
}

fn explain_graph_join_fanout_summary_line(
    query: &CypherQueryPlan,
    actual_rows: &HashMap<String, u64>,
) -> Option<String> {
    let metrics = graph_join_fanout_metrics(query, actual_rows);
    if metrics.compared_clauses == 0 {
        return None;
    }
    Some(format!(
        "Graph Join Fanout Summary: compared_clauses={}, risky_clauses={}, high_risk_clauses={}, max_fanout={:.3}, source=observed",
        metrics.compared_clauses,
        metrics.risky_clauses,
        metrics.high_risk_clauses,
        metrics.max_fanout
    ))
}

struct GraphJoinFanoutMetrics {
    compared_clauses: usize,
    risky_clauses: usize,
    high_risk_clauses: usize,
    max_fanout: f64,
}

struct GraphSummaryExplain {
    pivot_summary: GraphPivotSummaryMetrics,
    pivot_driver_metrics: GraphPivotDriverMetrics,
    pivot_metric_sources: GraphPivotMetricSources,
    actual_metric_sources: GraphActualMetricSources,
    access_metrics: GraphAccessSourceMetrics,
    procedure_metrics: GraphProcedureSourceMetrics,
    join_metrics: GraphJoinSummaryMetrics,
    drift_patterns: usize,
    high_drift_patterns: usize,
    risky_join_clauses: usize,
    high_risk_join_clauses: usize,
    max_fanout: Option<f64>,
    severity: &'static str,
    severity_reason: String,
    query_runtime_strategy: String,
    query_runtime_reason: String,
    query_runtime_source: String,
}

fn explain_graph_summary(
    query: &CypherQueryPlan,
    actual_rows: Option<&HashMap<String, u64>>,
    runtime_text: Option<&HashMap<String, String>>,
    txn_id: TxnId,
    executor: &Executor,
) -> GraphSummaryExplain {
    let pivot_summary = graph_pivot_summary_metrics(executor, txn_id, query, runtime_text);
    let pivot_driver_metrics = graph_pivot_driver_metrics(executor, txn_id, query, runtime_text);
    let pivot_metric_sources = graph_pivot_metric_sources(query, runtime_text);
    let actual_metric_sources = graph_actual_metric_sources(actual_rows);
    let access_metrics = graph_access_source_metrics(executor, txn_id, query);
    let procedure_metrics = graph_procedure_source_metrics(executor, txn_id, query);
    let join_metrics = graph_join_summary_metrics(query);
    let (drift_patterns, high_drift_patterns, join_fanout_metrics) =
        if let Some(actual_rows) = actual_rows {
            let (_, warning_count, high_warning_count) =
                explain_graph_drift_summary_line(txn_id, executor, query, actual_rows);
            (
                warning_count,
                high_warning_count,
                Some(graph_join_fanout_metrics(query, actual_rows)),
            )
        } else {
            (0, 0, None)
        };
    let risky_clauses = join_fanout_metrics
        .as_ref()
        .map_or(0, |metrics| metrics.risky_clauses);
    let high_risk_clauses = join_fanout_metrics
        .as_ref()
        .map_or(0, |metrics| metrics.high_risk_clauses);
    let max_fanout = join_fanout_metrics
        .as_ref()
        .map(|metrics| metrics.max_fanout);

    let (severity, severity_reason) = if high_drift_patterns > 0
        || high_risk_clauses > 0
        || (pivot_summary.fragile_pivots > 0
            && (drift_patterns > 0
                || risky_clauses > 0
                || join_metrics.correlated_non_shared > 0))
    {
        (
            "risk",
            format!(
                "fragile_pivots={}, high_drift_patterns={}, high_risk_join_clauses={}",
                pivot_summary.fragile_pivots, high_drift_patterns, high_risk_clauses
            ),
        )
    } else if pivot_summary.fragile_pivots > 0
        || drift_patterns > 0
        || risky_clauses > 0
        || pivot_summary.selected_non_leftmost > 0
        || join_metrics.correlated_non_shared > 0
        || join_metrics.independent_multi_scan > 0
    {
        (
            "watch",
            format!(
                "fragile_pivots={}, selected_non_leftmost={}, drift_patterns={}, risky_join_clauses={}, correlated_non_shared={}, independent_multi_scan={}",
                pivot_summary.fragile_pivots,
                pivot_summary.selected_non_leftmost,
                drift_patterns,
                risky_clauses,
                join_metrics.correlated_non_shared,
                join_metrics.independent_multi_scan
            ),
        )
    } else {
        ("ok", "no elevated graph planning signals".to_owned())
    };
    let (query_runtime_strategy, query_runtime_reason, query_runtime_source) =
        resolve_query_runtime_strategy(query, runtime_text);

    GraphSummaryExplain {
        pivot_summary,
        pivot_driver_metrics,
        pivot_metric_sources,
        actual_metric_sources,
        access_metrics,
        procedure_metrics,
        join_metrics,
        drift_patterns,
        high_drift_patterns,
        risky_join_clauses: risky_clauses,
        high_risk_join_clauses: high_risk_clauses,
        max_fanout,
        severity,
        severity_reason,
        query_runtime_strategy,
        query_runtime_reason,
        query_runtime_source,
    }
}

fn graph_join_fanout_metrics(
    query: &CypherQueryPlan,
    actual_rows: &HashMap<String, u64>,
) -> GraphJoinFanoutMetrics {
    let mut compared_clauses = 0usize;
    let mut risky_clauses = 0usize;
    let mut high_risk_clauses = 0usize;
    let mut max_fanout = 0.0f64;
    let mut available_vars = HashSet::new();

    let mut scan_clause =
        |clause_kind: &str, clause_index: usize, clause: &CypherMatchClause| {
            let multi_pattern = clause.patterns.len() > 1;
            let input_key = graph_access_clause_profile_input_key(clause_kind, clause_index);
            let output_key = graph_access_clause_profile_output_key(clause_kind, clause_index);
            let input_rows = actual_rows.get(&input_key).copied();
            let output_rows = actual_rows.get(&output_key).copied();
            if let (true, Some(input_raw), Some(output_raw)) =
                (multi_pattern, input_rows, output_rows)
            {
                compared_clauses += 1;
                let input = input_raw as f64;
                let output = output_raw as f64;
                if input > 0.0 {
                    max_fanout = max_fanout.max(output / input);
                }
                match graph_join_fanout_severity(input_rows, output_rows) {
                    Some("high") => {
                        risky_clauses += 1;
                        high_risk_clauses += 1;
                    }
                    Some(_) => risky_clauses += 1,
                    None => {}
                }
            }
            for pattern in &clause.patterns {
                available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
            }
        };

    for (clause_index, op) in query.pipeline.iter().enumerate() {
        if let CypherPipelineOp::Match(clause) = op {
            scan_clause("PipelineMatch", clause_index, clause);
        }
    }
    for (clause_index, clause) in query.matches.iter().enumerate() {
        scan_clause("Match", clause_index, clause);
    }

    GraphJoinFanoutMetrics {
        compared_clauses,
        risky_clauses,
        high_risk_clauses,
        max_fanout,
    }
}

fn estimated_clause_fanout(clause: &CypherMatchClause, txn_id: TxnId, executor: &Executor) -> Option<f64> {
    let input_rows = clause
        .patterns
        .first()
        .and_then(|pattern| executor.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern).estimated_rows)?;
    let output_rows = clause
        .patterns
        .last()
        .and_then(|pattern| executor.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern).estimated_rows)?;
    if input_rows == 0 {
        return None;
    }
    Some((output_rows as f64) / (input_rows as f64))
}

fn clause_join_shape(clause: &CypherMatchClause, available_vars: &HashSet<String>) -> &'static str {
    let correlated = clause_is_correlated(clause, available_vars);
    let shared_anchor = clause_is_shared_anchor(clause);
    match (correlated, shared_anchor) {
        (true, true) => "correlated_shared_anchor",
        (true, false) => "correlated_non_shared",
        (false, true) => "shared_anchor_uncorrelated",
        (false, false) => "independent_multi_scan",
    }
}

struct ClauseJoinRiskExplain {
    severity: &'static str,
    fanout_text: String,
    fanout_value: Option<f64>,
    basis: &'static str,
    join_risk_source: &'static str,
    correlated: bool,
    shared_anchor: bool,
    join_shape: &'static str,
    patterns: usize,
}

fn explain_clause_join_risk(
    clause: &CypherMatchClause,
    available_vars: &HashSet<String>,
    actual_input_rows: Option<u64>,
    actual_output_rows: Option<u64>,
    txn_id: TxnId,
    executor: &Executor,
) -> ClauseJoinRiskExplain {
    let correlated = clause_is_correlated(clause, available_vars);
    let shared_anchor = clause_is_shared_anchor(clause);
    let join_shape = clause_join_shape(clause, available_vars);
    let (severity, fanout_text, fanout_value, basis) =
        if let Some(severity) = graph_join_fanout_severity(actual_input_rows, actual_output_rows) {
            let fanout_value = ratio_value(actual_output_rows, actual_input_rows);
            (
                severity,
                fanout_value
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "unknown".to_owned()),
                fanout_value,
                "actual",
            )
        } else if actual_input_rows.is_some() && actual_output_rows.is_some() {
            let fanout_value = ratio_value(actual_output_rows, actual_input_rows);
            (
                "low",
                fanout_value
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "unknown".to_owned()),
                fanout_value,
                "actual",
            )
        } else if let Some(estimated_fanout) = estimated_clause_fanout(clause, txn_id, executor) {
            let severity = if estimated_fanout > 4.0 {
                "high"
            } else if estimated_fanout > 2.0 {
                "medium"
            } else {
                "low"
            };
            (
                severity,
                format!("{estimated_fanout:.3}"),
                Some(estimated_fanout),
                "estimated",
            )
        } else {
            ("unknown", "unknown".to_owned(), None, "unavailable")
        };

    ClauseJoinRiskExplain {
        severity,
        fanout_text,
        fanout_value,
        basis,
        join_risk_source: join_risk_source_from_basis(basis),
        correlated,
        shared_anchor,
        join_shape,
        patterns: clause.patterns.len(),
    }
}

fn explain_graph_summary_severity_line(
    summary: &GraphSummaryExplain,
    actual_rows_present: bool,
    runtime_text_present: bool,
) -> String {
    let source = match (actual_rows_present, runtime_text_present) {
        (true, true) => "mixed",
        (true, false) | (false, true) => "observed",
        (false, false) => "inferred",
    };
    format!(
        "Graph Summary Severity: severity={}, reason={}, source={}",
        summary.severity, summary.severity_reason, source
    )
}

fn build_graph_summary_json_payload_from_summary(
    summary: &GraphSummaryExplain,
) -> serde_json::Value {
    serde_json::json!({
        "query_runtime_strategy": summary.query_runtime_strategy.clone(),
        "query_runtime_reason": summary.query_runtime_reason.clone(),
        "query_runtime_source": summary.query_runtime_source,
        "severity": summary.severity,
        "pivotable_patterns": summary.pivot_summary.pivotable_patterns,
        "fragile_pivots": summary.pivot_summary.fragile_pivots,
        "blocked_pivots": summary.pivot_summary.blocked_pivots,
        "selected_non_leftmost": summary.pivot_summary.selected_non_leftmost,
        "selected_non_leftmost_source": summary.pivot_metric_sources.selected_non_leftmost_source,
        "cbo_pivoted": summary.pivot_driver_metrics.cbo_pivoted,
        "heuristic_pivoted": summary.pivot_driver_metrics.heuristic_pivoted,
        "pivot_driver_metrics_source": summary.pivot_metric_sources.pivot_driver_metrics_source,
        "drift_metrics_source": summary.actual_metric_sources.drift_metrics_source,
        "join_risk_metrics_source": summary.actual_metric_sources.join_risk_metrics_source,
        "row_store_source": summary.access_metrics.row_store_source,
        "traversal_store_source": summary.access_metrics.traversal_store_source,
        "projection_store_source": summary.access_metrics.projection_store_source,
        "hybrid_source": summary.access_metrics.hybrid_source,
        "row_fallback_patterns": summary.access_metrics.row_fallback_patterns,
        "row_store_traversal_patterns": summary.access_metrics.row_store_traversal_patterns,
        "total_procedures": summary.procedure_metrics.total_procedures,
        "procedure_projection_store_source": summary.procedure_metrics.projection_store_source,
        "row_fallback_procedures": summary.procedure_metrics.row_fallback_procedures,
        "weighted_projection_procedures": summary.procedure_metrics.weighted_projection_procedures,
        "multi_pattern_clauses": summary.join_metrics.multi_pattern_clauses,
        "correlated_clauses": summary.join_metrics.correlated_clauses,
        "shared_anchor_clauses": summary.join_metrics.shared_anchor_clauses,
        "max_correlated_vars_per_pattern": summary.join_metrics.max_correlated_vars_per_pattern,
        "correlated_shared_anchor": summary.join_metrics.correlated_shared_anchor,
        "correlated_non_shared": summary.join_metrics.correlated_non_shared,
        "shared_anchor_uncorrelated": summary.join_metrics.shared_anchor_uncorrelated,
        "independent_multi_scan": summary.join_metrics.independent_multi_scan,
        "drift_patterns": summary.drift_patterns,
        "high_drift_patterns": summary.high_drift_patterns,
        "risky_join_clauses": summary.risky_join_clauses,
        "high_risk_join_clauses": summary.high_risk_join_clauses,
        "max_fanout": summary.max_fanout,
    })
}

fn explain_graph_summary_metrics_line(
    summary: &GraphSummaryExplain,
) -> String {
    let max_fanout = if let Some(max_fanout) = summary.max_fanout {
        format!("{max_fanout:.3}")
    } else {
        "unknown".to_owned()
    };

    format!(
        "Graph Summary Metrics: severity={}, pivotable_patterns={}, fragile_pivots={}, blocked_pivots={}, selected_non_leftmost={}, selected_non_leftmost_source={}, cbo_pivoted={}, heuristic_pivoted={}, pivot_driver_metrics_source={}, drift_metrics_source={}, join_risk_metrics_source={}, row_store_source={}, traversal_store_source={}, projection_store_source={}, hybrid_source={}, row_fallback_patterns={}, row_store_traversal_patterns={}, total_procedures={}, procedure_projection_store_source={}, row_fallback_procedures={}, weighted_projection_procedures={}, multi_pattern_clauses={}, correlated_clauses={}, shared_anchor_clauses={}, max_correlated_vars_per_pattern={}, correlated_shared_anchor={}, correlated_non_shared={}, shared_anchor_uncorrelated={}, independent_multi_scan={}, drift_patterns={}, high_drift_patterns={}, risky_join_clauses={}, high_risk_join_clauses={}, max_fanout={}",
        summary.severity,
        summary.pivot_summary.pivotable_patterns,
        summary.pivot_summary.fragile_pivots,
        summary.pivot_summary.blocked_pivots,
        summary.pivot_summary.selected_non_leftmost,
        summary.pivot_metric_sources.selected_non_leftmost_source,
        summary.pivot_driver_metrics.cbo_pivoted,
        summary.pivot_driver_metrics.heuristic_pivoted,
        summary.pivot_metric_sources.pivot_driver_metrics_source,
        summary.actual_metric_sources.drift_metrics_source,
        summary.actual_metric_sources.join_risk_metrics_source,
        summary.access_metrics.row_store_source,
        summary.access_metrics.traversal_store_source,
        summary.access_metrics.projection_store_source,
        summary.access_metrics.hybrid_source,
        summary.access_metrics.row_fallback_patterns,
        summary.access_metrics.row_store_traversal_patterns,
        summary.procedure_metrics.total_procedures,
        summary.procedure_metrics.projection_store_source,
        summary.procedure_metrics.row_fallback_procedures,
        summary.procedure_metrics.weighted_projection_procedures,
        summary.join_metrics.multi_pattern_clauses,
        summary.join_metrics.correlated_clauses,
        summary.join_metrics.shared_anchor_clauses,
        summary.join_metrics.max_correlated_vars_per_pattern,
        summary.join_metrics.correlated_shared_anchor,
        summary.join_metrics.correlated_non_shared,
        summary.join_metrics.shared_anchor_uncorrelated,
        summary.join_metrics.independent_multi_scan,
        summary.drift_patterns,
        summary.high_drift_patterns,
        summary.risky_join_clauses,
        summary.high_risk_join_clauses,
        max_fanout
    )
}

fn ratio_value(numerator: Option<u64>, denominator: Option<u64>) -> Option<f64> {
    let numerator = numerator? as f64;
    let denominator = denominator? as f64;
    if denominator <= 0.0 {
        return None;
    }
    Some(numerator / denominator)
}

fn elapsed_ms_value(nanos: Option<u64>) -> Option<f64> {
    nanos.map(|value| (value as f64) / 1_000_000.0)
}

fn estimate_error_ratio_value(estimated_rows: Option<u64>, actual_rows: Option<u64>) -> Option<f64> {
    let estimated_rows = estimated_rows? as f64;
    let actual_rows = actual_rows? as f64;
    if actual_rows <= 0.0 {
        return None;
    }
    Some(estimated_rows / actual_rows)
}

fn option_debug_string<T: std::fmt::Debug>(value: Option<T>) -> Option<String> {
    value.map(|value| format!("{value:?}"))
}

pub(in crate::executor) fn explain_graph_pattern_hint_line(
    severity: &str,
    pattern: &CypherPattern,
    available_vars: &HashSet<String>,
) -> Option<String> {
    if severity != "high" {
        return None;
    }

    let seed_mode = explain_cypher_pattern_seed_binding_mode(pattern);
    let seed_binding_state = explain_cypher_pattern_seed_binding_state(pattern, available_vars);

    if seed_mode == "label_scan" {
        Some(
            "seed is using label_scan under high drift; check selective property indexes and graph statistics on the starting node"
                .to_owned(),
        )
    } else if seed_binding_state == "prebound" {
        Some(
            "seed is prebound under high drift; inspect correlated expansion fanout and whether an earlier pattern should narrow bindings sooner"
                .to_owned(),
        )
    } else {
        Some(
            "pattern shows high drift; review seed selectivity, edge-property filters, and stale graph statistics"
                .to_owned(),
        )
    }
}

impl Executor {
    fn cypher_procedure_uses_weighted_projection(
        call: &aiondb_plan::graph::CypherProcedureCall,
    ) -> bool {
        procedure_info(&call.procedure).is_some_and(|info| {
            call.args
                .iter()
                .zip(info.args.iter())
                .any(|(_, arg_info)| arg_info.config_field == AlgorithmConfigField::WeightColumn)
        })
    }

    pub(in crate::executor) fn describe_cypher_procedure_graph_plan(
        &self,
        txn_id: TxnId,
        call: &aiondb_plan::graph::CypherProcedureCall,
        clause_index: usize,
    ) -> CypherProcedureGraphAccessPlanHint {
        let weighted = Self::cypher_procedure_uses_weighted_projection(call);
        let projection_kind = if weighted { "weighted CSR" } else { "CSR" };
        let discovered = self.describe_current_cypher_projection_or_placeholder(txn_id, weighted);
        let projection = discovered.descriptor;
        let projection_ready = discovered.ready;
        CypherProcedureGraphAccessPlanHint {
            clause_index,
            procedure: call.procedure.clone(),
            weighted,
            projection: projection.clone(),
            projection_ready,
            plan: HybridGraphPlan {
                source: Some(HybridGraphSource::ProjectionStore),
                fallback_source: Some(HybridGraphSource::RowStore),
                estimated_rows: projection_ready.then_some(projection.stats.edge_count),
                projection_name: Some(projection.name),
                reason: Some(format!(
                    "native Cypher graph procedure executes against an executor-managed {projection_kind} projection snapshot"
                )),
            },
        }
    }

    pub(in crate::executor) fn describe_cypher_query_graph_procedure_plans(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
    ) -> Vec<CypherProcedureGraphAccessPlanHint> {
        query
            .pipeline
            .iter()
            .enumerate()
            .filter_map(|(clause_index, op)| match op {
                CypherPipelineOp::ProcedureCall(call) => {
                    Some(self.describe_cypher_procedure_graph_plan(txn_id, call, clause_index))
                }
                _ => None,
            })
            .collect()
    }

    fn describe_cypher_pattern_graph_plan_for_txn(
        &self,
        txn_id: TxnId,
        pattern: &CypherPattern,
    ) -> HybridGraphPlan {
        if pattern.relationships.is_empty() {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::RowStore),
                fallback_source: None,
                estimated_rows: None,
                projection_name: None,
                reason: Some("node-only Cypher pattern uses row-store scans".to_owned()),
            };
        }

        let mut available_edges = 0usize;
        let mut missing_edges = 0usize;
        let mut estimated_rows = 0u64;

        for rel in &pattern.relationships {
            let Some(table_id) = rel.table_id else {
                missing_edges = missing_edges.saturating_add(1);
                continue;
            };
            if self.storage_dml.adjacency_index_available(txn_id, table_id) {
                available_edges = available_edges.saturating_add(1);
                if let Some(stats) = self.storage_dml.adjacency_index_stats(txn_id, table_id) {
                    estimated_rows = estimated_rows.saturating_add(stats.edge_count);
                }
            } else {
                missing_edges = missing_edges.saturating_add(1);
            }
        }

        let estimated_rows = (estimated_rows > 0).then_some(estimated_rows);
        if available_edges == 0 {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::RowStore),
                fallback_source: None,
                estimated_rows,
                projection_name: None,
                reason: Some(
                    "relationship pattern has no native traversal store; row-store fallback only"
                        .to_owned(),
                ),
            };
        }
        if missing_edges == 0 {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::TraversalStore),
                fallback_source: Some(HybridGraphSource::RowStore),
                estimated_rows,
                projection_name: None,
                reason: Some(
                    "all relationship tables expose native adjacency traversal".to_owned(),
                ),
            };
        }
        HybridGraphPlan {
            source: Some(HybridGraphSource::Hybrid),
            fallback_source: Some(HybridGraphSource::RowStore),
            estimated_rows,
            projection_name: None,
            reason: Some(
                "some relationship tables expose native adjacency traversal and others fall back to row-store scans"
                    .to_owned(),
            ),
        }
    }

    pub(in crate::executor) fn describe_cypher_match_graph_plans(
        &self,
        context: &ExecutionContext,
        clause: &CypherMatchClause,
        clause_kind: CypherGraphAccessClauseKind,
        clause_index: usize,
    ) -> Vec<CypherGraphAccessPlanHint> {
        clause
            .patterns
            .iter()
            .enumerate()
            .map(|(pattern_index, pattern)| CypherGraphAccessPlanHint {
                clause_kind: clause_kind.clone(),
                clause_index,
                pattern_index,
                plan: self.describe_cypher_pattern_graph_plan_for_txn(context.txn_id, pattern),
            })
            .collect()
    }

    pub(in crate::executor) fn describe_cypher_query_graph_plans(
        &self,
        context: &ExecutionContext,
        query: &CypherQueryPlan,
    ) -> Vec<CypherGraphAccessPlanHint> {
        let mut hints = Vec::new();
        for (clause_index, op) in query.pipeline.iter().enumerate() {
            if let CypherPipelineOp::Match(clause) = op {
                hints.extend(self.describe_cypher_match_graph_plans(
                    context,
                    clause,
                    CypherGraphAccessClauseKind::PipelineMatch,
                    clause_index,
                ));
            }
        }
        for (clause_index, clause) in query.matches.iter().enumerate() {
            hints.extend(self.describe_cypher_match_graph_plans(
                context,
                clause,
                CypherGraphAccessClauseKind::Match,
                clause_index,
            ));
        }
        hints
    }

    pub(in crate::executor) fn describe_cypher_pattern_graph_plan(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> HybridGraphPlan {
        self.describe_cypher_pattern_graph_plan_for_txn(context.txn_id, pattern)
    }

    pub fn explain_cypher_query_graph_access_lines(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
        actual_rows: Option<&HashMap<String, u64>>,
        elapsed_nanos: Option<&HashMap<String, u64>>,
        runtime_text: Option<&HashMap<String, String>>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut available_vars = HashSet::new();
        let summary = explain_graph_summary(query, actual_rows, runtime_text, txn_id, self);
        lines.push(explain_cypher_query_summary_line(query));
        lines.push(format!(
            "Graph Query Runtime: strategy={}, reason={}, source={}",
            summary.query_runtime_strategy,
            summary.query_runtime_reason,
            summary.query_runtime_source
        ));
        lines.push(explain_graph_summary_severity_line(
            &summary,
            actual_rows.is_some(),
            runtime_text.is_some(),
        ));
        lines.push(explain_graph_summary_metrics_line(&summary));
        lines.push(format!(
            "Graph Summary JSON: {}",
            build_graph_summary_json_payload_from_summary(&summary)
        ));
        if let Some(access_summary) = explain_graph_access_summary_line(self, txn_id, query) {
            lines.push(access_summary);
        }
        if let Some(procedure_summary) = explain_graph_procedure_summary_line(self, txn_id, query) {
            lines.push(procedure_summary);
        }
        if let Some(access_warning) = explain_graph_access_warning_line(self, txn_id, query) {
            lines.push(access_warning);
        }
        lines.push(format!(
            "Graph Detail JSON: {}",
            self.explain_cypher_query_graph_detail_json(
                txn_id,
                query,
                actual_rows,
                elapsed_nanos,
                runtime_text,
            )
        ));
        if let Some(pivot_summary) =
            explain_graph_pivot_summary_line(self, txn_id, query, runtime_text)
        {
            lines.push(pivot_summary);
        }
        if let Some(pivot_hint) = explain_graph_pivot_hint_line(self, txn_id, query, runtime_text) {
            lines.push(pivot_hint);
        }
        if let Some(pivot_note) = explain_graph_pivot_note_line(self, txn_id, query, runtime_text) {
            lines.push(pivot_note);
        }
        if let Some(planner_warning) =
            explain_graph_planner_warning_line(self, txn_id, query, runtime_text)
        {
            lines.push(planner_warning);
        }
        if let Some(join_summary) = explain_graph_join_summary_line(query) {
            lines.push(join_summary);
        }
        if let Some(join_hint) = explain_graph_join_hint_line(query) {
            lines.push(join_hint);
        }
        if let Some(actual_rows) = actual_rows {
            if let Some(join_fanout_summary) =
                explain_graph_join_fanout_summary_line(query, actual_rows)
            {
                lines.push(join_fanout_summary);
            }
            let (summary, warning_count, high_warning_count) = explain_graph_drift_summary_line(
                txn_id,
                self,
                query,
                actual_rows,
            );
            lines.push(summary);
            if let Some(suggestion) =
                explain_graph_drift_suggestion_line(warning_count, high_warning_count)
            {
                lines.push(suggestion);
            }
            if let Some(hint) = explain_graph_plan_hint_line(high_warning_count) {
                lines.push(hint);
            }
        }
        for hint in self.describe_cypher_query_graph_procedure_plans(txn_id, query) {
            lines.push(format!(
                "Graph Projection [ProcedureCall {}]: procedure={}, source={:?}, fallback={:?}, projection={}, snapshot_generation={}, refresh_policy={:?}, refreshed_at_epoch_millis={}, weighted={}, estimated_rows={}, node_count={}, edge_count={}, reason={}",
                hint.clause_index,
                hint.procedure,
                hint.plan.source,
                hint.plan.fallback_source,
                hint.plan.projection_name.as_deref().unwrap_or("unknown"),
                hint.projection.snapshot.generation,
                hint.projection.snapshot.refresh_policy,
                hint.projection
                    .snapshot
                    .refreshed_at_epoch_millis
                    .map_or_else(|| "unknown".to_owned(), |ts| ts.to_string()),
                hint.weighted,
                hint.plan
                    .estimated_rows
                    .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
                (hint.projection_ready)
                    .then_some(hint.projection.stats)
                    .and_then(|stats| stats.node_count)
                    .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
                (hint.projection_ready)
                    .then_some(hint.projection.stats)
                    .map_or_else(|| "unknown".to_owned(), |stats| stats.edge_count.to_string()),
                hint.plan.reason.unwrap_or_default()
            ));
        }
        for (clause_index, op) in query.pipeline.iter().enumerate() {
            if let CypherPipelineOp::Match(clause) = op {
                let actual_input_rows_value = actual_rows.and_then(|rows| {
                    rows.get(&graph_access_clause_profile_input_key("PipelineMatch", clause_index))
                        .copied()
                });
                let actual_output_rows_value = actual_rows.and_then(|rows| {
                    rows.get(&graph_access_clause_profile_output_key(
                        "PipelineMatch",
                        clause_index,
                    ))
                    .copied()
                });
                let actual_input_rows = actual_input_rows_value
                    .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string());
                let actual_output_rows = actual_output_rows_value
                    .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string());
                let actual_selectivity =
                    explain_selectivity_ratio(actual_output_rows_value, actual_input_rows_value);
                let actual_time_ms = explain_elapsed_ms(elapsed_nanos.and_then(|times| {
                    times.get(&graph_access_clause_profile_time_key("PipelineMatch", clause_index))
                        .copied()
                }));
            let clause_runtime =
                explain_clause_runtime("PipelineMatch", clause_index, clause, runtime_text);
            lines.push(format!(
                "Graph Clause [{} {}]: actual_input_rows={}, actual_output_rows={}, actual_selectivity={}, actual_time_ms={}, optional={}, patterns={}, runtime_strategy={}, runtime_strategy_reason={}, runtime_strategy_reason_source={}, runtime_strategy_blocker={}, runtime_strategy_blocker_source={}, runtime_strategy_source={}",
                "PipelineMatch",
                clause_index,
                actual_input_rows,
                actual_output_rows,
                actual_selectivity,
                actual_time_ms,
                clause.optional,
                clause.patterns.len(),
                clause_runtime.strategy,
                clause_runtime.reason,
                clause_runtime.reason_source,
                clause_runtime.blocker.as_deref().unwrap_or("none"),
                clause_runtime.blocker_source,
                clause_runtime.strategy_source,
            ));
                if clause.patterns.len() > 1 {
                    let join_risk = explain_clause_join_risk(
                        clause,
                        &available_vars,
                        actual_input_rows_value,
                        actual_output_rows_value,
                        txn_id,
                        self,
                    );
                    lines.push(format!(
                        "Graph Join Risk [{} {}]: severity={}, fanout={}, basis={}, join_risk_source={}, correlated={}, correlated_source=inferred, shared_anchor={}, shared_anchor_source=inferred, join_shape={}, join_shape_source=inferred, patterns={}",
                        "PipelineMatch",
                        clause_index,
                        join_risk.severity,
                        join_risk.fanout_text,
                        join_risk.basis,
                        join_risk.join_risk_source,
                        join_risk.correlated,
                        join_risk.shared_anchor,
                        join_risk.join_shape,
                        join_risk.patterns,
                    ));
                }
                for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                    let plan = self.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern);
                    let actual_rows_value = actual_rows
                        .and_then(|rows| {
                            rows.get(&graph_access_profile_key(
                                "PipelineMatch",
                                clause_index,
                                pattern_index,
                            ))
                        })
                        .copied();
                    let pattern_metrics = explain_pattern_metrics(
                        plan.estimated_rows,
                        actual_rows_value,
                        actual_input_rows_value,
                        elapsed_nanos.and_then(|times| {
                            times.get(&graph_access_pattern_profile_time_key(
                                "PipelineMatch",
                                clause_index,
                                pattern_index,
                            ))
                            .copied()
                        }),
                    );
                    let pattern_runtime = explain_pattern_runtime(
                        self,
                        txn_id,
                        pattern,
                        "PipelineMatch",
                        clause_index,
                        pattern_index,
                        runtime_text,
                    );
                    let static_fields =
                        PatternStaticExplainFields::gather(pattern, &available_vars);
                    lines.push(format!(
                        "Graph Access [{} {} pattern {}]: source={:?}, fallback={:?}, estimated_rows={}, actual_rows={}, estimate_error_ratio={}, estimated_selectivity={}, actual_selectivity={}, actual_time_ms={}, optional={}, nodes={}, rels={}, pattern_runtime_strategy={}, pattern_runtime_strategy_source={}, pattern_runtime_reason={}, pattern_runtime_reason_source={}, seed={}, seed_binding_state={}, seed_binding_state_source={}, correlated_vars={}, correlated_vars_source={}, seed_mode={}, seed_mode_source={}, seed_constraints={}, seed_constraints_source={}, pivot_driver={}, pivot_driver_source={}, pivot_reason={}, pivot_reason_source={}, pivot_decision={}, pivot_decision_source={}, pivot_margin={}, pivot_competition={}, pivot_scores={}, first_rel={}, first_rel_source={}, first_rel_mode={}, first_rel_mode_source={}, first_rel_constraints={}, first_rel_constraints_source={}, bound_vars={}, bound_vars_source={}, flags={}, flags_source={}, shape={}, shape_source={}, reason={}",
                        "PipelineMatch",
                        clause_index,
                        pattern_index,
                        plan.source,
                        plan.fallback_source,
                        pattern_metrics.estimated_rows_text,
                        pattern_metrics.actual_rows_text,
                        pattern_metrics.estimate_error_ratio_text,
                        pattern_metrics.estimated_selectivity_text,
                        pattern_metrics.actual_selectivity_text,
                        pattern_metrics.actual_time_ms_text,
                        clause.optional,
                        pattern.nodes.len(),
                        pattern.relationships.len(),
                        pattern_runtime.strategy,
                        pattern_runtime.strategy_source,
                        pattern_runtime.reason,
                        pattern_runtime.reason_source,
                        static_fields.seed,
                        static_fields.seed_binding_state,
                        INFERRED_SOURCE,
                        static_fields.correlated_vars,
                        INFERRED_SOURCE,
                        static_fields.seed_mode,
                        INFERRED_SOURCE,
                        static_fields.seed_constraints,
                        INFERRED_SOURCE,
                        pattern_runtime.pivot_driver,
                        pattern_runtime.pivot_driver_source,
                        pattern_runtime.pivot_reason,
                        pattern_runtime.pivot_reason_source,
                        pattern_runtime.pivot_decision,
                        pattern_runtime.pivot_decision_source,
                        explain_cypher_pattern_pivot_margin(pattern),
                        explain_cypher_pattern_pivot_competition(pattern),
                        explain_cypher_pattern_pivot_scores(pattern),
                        static_fields.first_rel,
                        INFERRED_SOURCE,
                        static_fields.first_rel_mode,
                        INFERRED_SOURCE,
                        static_fields.first_rel_constraints,
                        INFERRED_SOURCE,
                        static_fields.bound_vars,
                        INFERRED_SOURCE,
                        static_fields.flags,
                        INFERRED_SOURCE,
                        static_fields.shape,
                        INFERRED_SOURCE,
                        plan.reason.unwrap_or_default()
                    ));
                    if let Some(severity) = graph_estimate_warning_severity(
                        pattern_metrics.estimated_rows_value,
                        pattern_metrics.actual_rows_value,
                    )
                    {
                        lines.push(format!(
                            "Graph Warning [{} {} pattern {}]: severity={}, issue=estimate_drift, estimated_rows={}, actual_rows={}, estimate_error_ratio={}",
                            "PipelineMatch",
                            clause_index,
                            pattern_index,
                            severity,
                            pattern_metrics.estimated_rows_text,
                            pattern_metrics.actual_rows_text,
                            pattern_metrics.estimate_error_ratio_text,
                        ));
                        if let Some(hint) =
                            explain_graph_pattern_hint_line(severity, pattern, &available_vars)
                        {
                            lines.push(format!(
                                "Graph Pattern Hint [{} {} pattern {}]: {}",
                                "PipelineMatch", clause_index, pattern_index, hint
                            ));
                        }
                    }
                    available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
                }
            }
        }
        for (clause_index, clause) in query.matches.iter().enumerate() {
            let actual_input_rows_value = actual_rows
                .and_then(|rows| rows.get(&graph_access_clause_profile_input_key("Match", clause_index)))
                .copied();
            let actual_output_rows_value = actual_rows
                .and_then(|rows| rows.get(&graph_access_clause_profile_output_key("Match", clause_index)))
                .copied();
            let actual_input_rows = actual_input_rows_value
                .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string());
            let actual_output_rows = actual_output_rows_value
                .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string());
            let actual_selectivity =
                explain_selectivity_ratio(actual_output_rows_value, actual_input_rows_value);
            let actual_time_ms = explain_elapsed_ms(elapsed_nanos.and_then(|times| {
                times.get(&graph_access_clause_profile_time_key("Match", clause_index))
                    .copied()
            }));
            let clause_runtime =
                explain_clause_runtime("Match", clause_index, clause, runtime_text);
            lines.push(format!(
                "Graph Clause [{} {}]: actual_input_rows={}, actual_output_rows={}, actual_selectivity={}, actual_time_ms={}, optional={}, patterns={}, runtime_strategy={}, runtime_strategy_reason={}, runtime_strategy_reason_source={}, runtime_strategy_blocker={}, runtime_strategy_blocker_source={}, runtime_strategy_source={}",
                "Match",
                clause_index,
                actual_input_rows,
                actual_output_rows,
                actual_selectivity,
                actual_time_ms,
                clause.optional,
                clause.patterns.len(),
                clause_runtime.strategy,
                clause_runtime.reason,
                clause_runtime.reason_source,
                clause_runtime.blocker.as_deref().unwrap_or("none"),
                clause_runtime.blocker_source,
                clause_runtime.strategy_source,
            ));
            if clause.patterns.len() > 1 {
                let join_risk = explain_clause_join_risk(
                    clause,
                    &available_vars,
                    actual_input_rows_value,
                    actual_output_rows_value,
                    txn_id,
                    self,
                );
                lines.push(format!(
                    "Graph Join Risk [{} {}]: severity={}, fanout={}, basis={}, join_risk_source={}, correlated={}, correlated_source=inferred, shared_anchor={}, shared_anchor_source=inferred, join_shape={}, join_shape_source=inferred, patterns={}",
                    "Match",
                    clause_index,
                    join_risk.severity,
                    join_risk.fanout_text,
                    join_risk.basis,
                    join_risk.join_risk_source,
                    join_risk.correlated,
                    join_risk.shared_anchor,
                    join_risk.join_shape,
                    join_risk.patterns,
                ));
            }
            for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                let plan = self.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern);
                let actual_rows_value = actual_rows
                    .and_then(|rows| {
                        rows.get(&graph_access_profile_key("Match", clause_index, pattern_index))
                    })
                    .copied();
                let pattern_metrics = explain_pattern_metrics(
                    plan.estimated_rows,
                    actual_rows_value,
                    actual_input_rows_value,
                    elapsed_nanos.and_then(|times| {
                        times.get(&graph_access_pattern_profile_time_key(
                            "Match",
                            clause_index,
                            pattern_index,
                        ))
                        .copied()
                    }),
                );
                let pattern_runtime = explain_pattern_runtime(
                    self,
                    txn_id,
                    pattern,
                    "Match",
                    clause_index,
                    pattern_index,
                    runtime_text,
                );
                let static_fields = PatternStaticExplainFields::gather(pattern, &available_vars);
                lines.push(format!(
                    "Graph Access [{} {} pattern {}]: source={:?}, fallback={:?}, estimated_rows={}, actual_rows={}, estimate_error_ratio={}, estimated_selectivity={}, actual_selectivity={}, actual_time_ms={}, optional={}, nodes={}, rels={}, pattern_runtime_strategy={}, pattern_runtime_strategy_source={}, pattern_runtime_reason={}, pattern_runtime_reason_source={}, seed={}, seed_binding_state={}, seed_binding_state_source={}, correlated_vars={}, correlated_vars_source={}, seed_mode={}, seed_mode_source={}, seed_constraints={}, seed_constraints_source={}, pivot_driver={}, pivot_driver_source={}, pivot_reason={}, pivot_reason_source={}, pivot_decision={}, pivot_decision_source={}, pivot_margin={}, pivot_competition={}, pivot_scores={}, first_rel={}, first_rel_source={}, first_rel_mode={}, first_rel_mode_source={}, first_rel_constraints={}, first_rel_constraints_source={}, bound_vars={}, bound_vars_source={}, flags={}, flags_source={}, shape={}, shape_source={}, reason={}",
                    "Match",
                    clause_index,
                    pattern_index,
                    plan.source,
                    plan.fallback_source,
                    pattern_metrics.estimated_rows_text,
                    pattern_metrics.actual_rows_text,
                    pattern_metrics.estimate_error_ratio_text,
                    pattern_metrics.estimated_selectivity_text,
                    pattern_metrics.actual_selectivity_text,
                    pattern_metrics.actual_time_ms_text,
                    clause.optional,
                    pattern.nodes.len(),
                    pattern.relationships.len(),
                    pattern_runtime.strategy,
                    pattern_runtime.strategy_source,
                    pattern_runtime.reason,
                    pattern_runtime.reason_source,
                    static_fields.seed,
                    static_fields.seed_binding_state,
                    INFERRED_SOURCE,
                    static_fields.correlated_vars,
                    INFERRED_SOURCE,
                    static_fields.seed_mode,
                    INFERRED_SOURCE,
                    static_fields.seed_constraints,
                    INFERRED_SOURCE,
                    pattern_runtime.pivot_driver,
                    pattern_runtime.pivot_driver_source,
                    pattern_runtime.pivot_reason,
                    pattern_runtime.pivot_reason_source,
                    pattern_runtime.pivot_decision,
                    pattern_runtime.pivot_decision_source,
                    explain_cypher_pattern_pivot_margin(pattern),
                    explain_cypher_pattern_pivot_competition(pattern),
                    explain_cypher_pattern_pivot_scores(pattern),
                    static_fields.first_rel,
                    INFERRED_SOURCE,
                    static_fields.first_rel_mode,
                    INFERRED_SOURCE,
                    static_fields.first_rel_constraints,
                    INFERRED_SOURCE,
                    static_fields.bound_vars,
                    INFERRED_SOURCE,
                    static_fields.flags,
                    INFERRED_SOURCE,
                    static_fields.shape,
                    INFERRED_SOURCE,
                    plan.reason.unwrap_or_default()
                ));
                if let Some(severity) = graph_estimate_warning_severity(
                    pattern_metrics.estimated_rows_value,
                    pattern_metrics.actual_rows_value,
                )
                {
                    lines.push(format!(
                        "Graph Warning [{} {} pattern {}]: severity={}, issue=estimate_drift, estimated_rows={}, actual_rows={}, estimate_error_ratio={}",
                        "Match",
                        clause_index,
                        pattern_index,
                        severity,
                        pattern_metrics.estimated_rows_text,
                        pattern_metrics.actual_rows_text,
                        pattern_metrics.estimate_error_ratio_text,
                    ));
                    if let Some(hint) =
                        explain_graph_pattern_hint_line(severity, pattern, &available_vars)
                    {
                        lines.push(format!(
                            "Graph Pattern Hint [{} {} pattern {}]: {}",
                            "Match", clause_index, pattern_index, hint
                        ));
                    }
                }
                available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
            }
        }
        lines
    }

    pub fn explain_cypher_query_graph_summary_json(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
        actual_rows: Option<&HashMap<String, u64>>,
        runtime_text: Option<&HashMap<String, String>>,
    ) -> serde_json::Value {
        let summary = explain_graph_summary(query, actual_rows, runtime_text, txn_id, self);
        build_graph_summary_json_payload_from_summary(&summary)
    }

    pub fn explain_cypher_query_graph_detail_json(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
        actual_rows: Option<&HashMap<String, u64>>,
        elapsed_nanos: Option<&HashMap<String, u64>>,
        runtime_text: Option<&HashMap<String, String>>,
    ) -> serde_json::Value {
        let summary = explain_graph_summary(query, actual_rows, runtime_text, txn_id, self);
        let mut available_vars = HashSet::new();
        let mut clauses = Vec::new();

        let mut push_clause = |kind: &str,
                               clause_index: usize,
                               clause: &CypherMatchClause,
                               available_vars: &mut HashSet<String>| {
            let clause_runtime =
                explain_clause_runtime(kind, clause_index, clause, runtime_text);
            let actual_input_rows = actual_rows
                .and_then(|rows| rows.get(&graph_access_clause_profile_input_key(kind, clause_index)))
                .copied();
            let actual_output_rows = actual_rows
                .and_then(|rows| rows.get(&graph_access_clause_profile_output_key(kind, clause_index)))
                .copied();
            let actual_selectivity = ratio_value(actual_output_rows, actual_input_rows);
            let actual_time_ms = elapsed_ms_value(elapsed_nanos.and_then(|times| {
                times
                    .get(&graph_access_clause_profile_time_key(kind, clause_index))
                    .copied()
            }));

            let join_risk = if clause.patterns.len() > 1 {
                let join_risk = explain_clause_join_risk(
                    clause,
                    available_vars,
                    actual_input_rows,
                    actual_output_rows,
                    txn_id,
                    self,
                );
                Some(serde_json::json!({
                    "severity": join_risk.severity,
                    "fanout": join_risk.fanout_value,
                    "basis": join_risk.basis,
                    "join_risk_source": join_risk.join_risk_source,
                    "correlated": join_risk.correlated,
                    "correlated_source": "inferred",
                    "shared_anchor": join_risk.shared_anchor,
                    "shared_anchor_source": "inferred",
                    "join_shape": join_risk.join_shape,
                    "join_shape_source": "inferred",
                    "patterns": join_risk.patterns,
                }))
            } else {
                None
            };

            let mut pattern_values = Vec::new();
            for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                let plan = self.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern);
                let actual_pattern_rows = actual_rows
                    .and_then(|rows| rows.get(&graph_access_profile_key(kind, clause_index, pattern_index)))
                    .copied();
                let pattern_metrics = explain_pattern_metrics(
                    plan.estimated_rows,
                    actual_pattern_rows,
                    actual_input_rows,
                    elapsed_nanos.and_then(|times| {
                        times
                            .get(&graph_access_pattern_profile_time_key(kind, clause_index, pattern_index))
                            .copied()
                    }),
                );
                let pattern_runtime = explain_pattern_runtime(
                    self,
                    txn_id,
                    pattern,
                    kind,
                    clause_index,
                    pattern_index,
                    runtime_text,
                );
                let warning_severity = graph_estimate_warning_severity(
                    pattern_metrics.estimated_rows_value,
                    pattern_metrics.actual_rows_value,
                );
                let static_fields = PatternStaticExplainFields::gather(pattern, available_vars);
                pattern_values.push(serde_json::json!({
                    "kind": kind,
                    "clause_index": clause_index,
                    "pattern_index": pattern_index,
                    "source": option_debug_string(plan.source),
                    "fallback": option_debug_string(plan.fallback_source),
                    "estimated_rows": pattern_metrics.estimated_rows_value,
                    "actual_rows": pattern_metrics.actual_rows_value,
                    "estimate_error_ratio": pattern_metrics.estimate_error_ratio_value,
                    "estimated_selectivity": pattern_metrics.estimated_selectivity_value,
                    "actual_selectivity": pattern_metrics.actual_selectivity_value,
                    "actual_time_ms": pattern_metrics.actual_time_ms_value,
                    "optional": clause.optional,
                    "nodes": pattern.nodes.len(),
                    "rels": pattern.relationships.len(),
                    "pattern_runtime_strategy": pattern_runtime.strategy,
                    "pattern_runtime_strategy_source": pattern_runtime.strategy_source,
                    "pattern_runtime_reason": pattern_runtime.reason,
                    "pattern_runtime_reason_source": pattern_runtime.reason_source,
                    "seed": static_fields.seed,
                    "seed_binding_state": static_fields.seed_binding_state,
                    "seed_binding_state_source": INFERRED_SOURCE,
                    "correlated_vars": static_fields.correlated_vars,
                    "correlated_vars_source": INFERRED_SOURCE,
                    "seed_mode": static_fields.seed_mode,
                    "seed_mode_source": INFERRED_SOURCE,
                    "seed_constraints": static_fields.seed_constraints,
                    "seed_constraints_source": INFERRED_SOURCE,
                    "pivot_driver": pattern_runtime.pivot_driver,
                    "pivot_driver_source": pattern_runtime.pivot_driver_source,
                    "pivot_reason": pattern_runtime.pivot_reason,
                    "pivot_reason_source": pattern_runtime.pivot_reason_source,
                    "pivot_decision": pattern_runtime.pivot_decision,
                    "pivot_decision_source": pattern_runtime.pivot_decision_source,
                    "pivot_margin": explain_cypher_pattern_pivot_margin(pattern),
                    "pivot_competition": explain_cypher_pattern_pivot_competition(pattern),
                    "pivot_scores": explain_cypher_pattern_pivot_scores(pattern),
                    "first_rel": static_fields.first_rel,
                    "first_rel_source": INFERRED_SOURCE,
                    "first_rel_mode": static_fields.first_rel_mode,
                    "first_rel_mode_source": INFERRED_SOURCE,
                    "first_rel_constraints": static_fields.first_rel_constraints,
                    "first_rel_constraints_source": INFERRED_SOURCE,
                    "bound_vars": static_fields.bound_vars,
                    "bound_vars_source": INFERRED_SOURCE,
                    "flags": static_fields.flags,
                    "flags_source": INFERRED_SOURCE,
                    "shape": static_fields.shape,
                    "shape_source": INFERRED_SOURCE,
                    "reason": plan.reason.unwrap_or_default(),
                    "warning_severity": warning_severity,
                }));
                available_vars.extend(collect_cypher_pattern_bound_vars(pattern));
            }

            clauses.push(serde_json::json!({
                "kind": kind,
                "clause_index": clause_index,
                "optional": clause.optional,
                "patterns": clause.patterns.len(),
                "runtime_strategy": clause_runtime.strategy,
                "runtime_strategy_reason": clause_runtime.reason,
                "runtime_strategy_reason_source": clause_runtime.reason_source,
                "runtime_strategy_blocker": clause_runtime.blocker,
                "runtime_strategy_blocker_source": clause_runtime.blocker_source,
                "runtime_strategy_source": clause_runtime.strategy_source,
                "actual_input_rows": actual_input_rows,
                "actual_output_rows": actual_output_rows,
                "actual_selectivity": actual_selectivity,
                "actual_time_ms": actual_time_ms,
                "join_risk": join_risk,
                "pattern_details": pattern_values,
            }));
        };

        for (clause_index, op) in query.pipeline.iter().enumerate() {
            if let CypherPipelineOp::Match(clause) = op {
                push_clause("PipelineMatch", clause_index, clause, &mut available_vars);
            }
        }
        for (clause_index, clause) in query.matches.iter().enumerate() {
            push_clause("Match", clause_index, clause, &mut available_vars);
        }

        serde_json::json!({
            "summary": build_graph_summary_json_payload_from_summary(&summary),
            "clauses": clauses,
        })
    }

    pub fn explain_physical_plan_graph_access_lines(
        &self,
        txn_id: TxnId,
        plan: &aiondb_plan::PhysicalPlan,
        actual_rows: Option<&HashMap<String, u64>>,
        elapsed_nanos: Option<&HashMap<String, u64>>,
        runtime_text: Option<&HashMap<String, String>>,
    ) -> Vec<String> {
        fn collect(
            executor: &Executor,
            txn_id: TxnId,
            plan: &aiondb_plan::PhysicalPlan,
            actual_rows: Option<&HashMap<String, u64>>,
            elapsed_nanos: Option<&HashMap<String, u64>>,
            runtime_text: Option<&HashMap<String, String>>,
            lines: &mut Vec<String>,
        ) {
            match plan {
                aiondb_plan::PhysicalPlan::CypherQuery(query) => {
                    lines.extend(
                        executor.explain_cypher_query_graph_access_lines(
                            txn_id,
                            query.as_ref(),
                            actual_rows,
                            elapsed_nanos,
                            runtime_text,
                        ),
                    );
                }
                aiondb_plan::PhysicalPlan::ProjectSource { source, .. }
                | aiondb_plan::PhysicalPlan::AggregateSource { source, .. }
                | aiondb_plan::PhysicalPlan::PartialAggregate { source, .. }
                | aiondb_plan::PhysicalPlan::CreateTableAs { source, .. }
                | aiondb_plan::PhysicalPlan::InsertSelect { source, .. } => {
                    collect(
                        executor,
                        txn_id,
                        source,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                }
                aiondb_plan::PhysicalPlan::NestedLoopJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::HashJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::MergeJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::SetOperation { left, right, .. }
                | aiondb_plan::PhysicalPlan::BroadcastHashJoin {
                    broadcast: left,
                    local: right,
                    ..
                } => {
                    collect(
                        executor,
                        txn_id,
                        left,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                    collect(
                        executor,
                        txn_id,
                        right,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                }
                aiondb_plan::PhysicalPlan::NestedLoopIndexJoin { left, .. } => {
                    collect(
                        executor,
                        txn_id,
                        left,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                }
                aiondb_plan::PhysicalPlan::DistributedAppend { fragments, .. } => {
                    for fragment in fragments {
                        collect(
                            executor,
                            txn_id,
                            fragment,
                            actual_rows,
                            elapsed_nanos,
                            runtime_text,
                            lines,
                        );
                    }
                }
                aiondb_plan::PhysicalPlan::RecursiveCte {
                    base, recursive, ..
                } => {
                    collect(
                        executor,
                        txn_id,
                        base,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                    collect(
                        executor,
                        txn_id,
                        recursive,
                        actual_rows,
                        elapsed_nanos,
                        runtime_text,
                        lines,
                    );
                }
                aiondb_plan::PhysicalPlan::FinalAggregate { partials, .. } => {
                    for partial in partials {
                        collect(
                            executor,
                            txn_id,
                            partial,
                            actual_rows,
                            elapsed_nanos,
                            runtime_text,
                            lines,
                        );
                    }
                }
                _ => {}
            }
        }

        let mut lines = Vec::new();
        collect(
            self,
            txn_id,
            plan,
            actual_rows,
            elapsed_nanos,
            runtime_text,
            &mut lines,
        );
        lines
    }
}
