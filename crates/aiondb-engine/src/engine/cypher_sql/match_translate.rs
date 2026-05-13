//! MATCH clause translation.

#![allow(
    clippy::items_after_statements,
    clippy::match_same_arms,
    clippy::single_match_else,
    clippy::too_many_lines
)]

use aiondb_parser::ast::Expr;
use aiondb_parser::cypher_ast::{CypherClause, CypherDirection, CypherReturnClause};
use tracing::warn;

use super::escape::{escape_sq, qi, validate_rel_type};

/// Build a SQL condition that checks whether `alias.__labels` contains `label`.
fn label_check(alias: &str, label: &str) -> String {
    format!("'{}' = ANY({alias}.\"__labels\")", escape_sq(label))
}
use super::expr::{
    append_order_by, append_skip_limit, cypher_return_alias, expr_to_sql_plain, match_expr_to_sql,
    match_return_item_to_sql, order_by_suffix,
};
use super::{CYPHER_EDGES_TABLE, CYPHER_NODES_TABLE};

/// Maximum number of hops for variable-length relationship traversal.
/// Prevents SQL UNION explosion: each additional hop adds another UNION branch
/// with an N-way self-join on the edges table, so cost grows as O(N * |E|^N).
/// 10 hops is sufficient for most real-world graph traversals while keeping
/// generated SQL within planner limits.
const MAX_VAR_LENGTH_HOPS: u32 = 10;

pub(crate) fn translate_match_pipeline(clauses: &[CypherClause]) -> Option<String> {
    if clauses.iter().any(|c| matches!(c, CypherClause::Set(_))) {
        return super::mutate_translate::translate_match_set_pipeline(clauses);
    }
    if clauses.iter().any(|c| matches!(c, CypherClause::Delete(_))) {
        return super::mutate_translate::translate_match_delete_pipeline(clauses);
    }
    if clauses.iter().any(|c| matches!(c, CypherClause::Remove(_))) {
        return super::mutate_translate::translate_match_remove_pipeline(clauses);
    }
    translate_match_return_pipeline(clauses)
}

/// Build a SQL `__type` filter for a relationship alias.
///
/// Returns:
/// - `Some(Some("alias.\"__type\" = '...' "))` for a single rel_type;
/// - `Some(Some("alias.\"__type\" IN (...)"))` for multi-type alternatives;
/// - `Some(None)` when `rel.rel_type` is unset (caller decides if a "true"
///   placeholder is needed);
/// - `None` if any rel_type fails validation (caller bails out).
fn rel_type_filter(
    rel: &aiondb_parser::cypher_ast::CypherRelPattern,
    alias: &str,
) -> Option<Option<String>> {
    let Some(ref rt) = rel.rel_type else {
        return Some(None);
    };
    if validate_rel_type(rt).is_err() {
        warn!("cypher translate: invalid relationship type '{rt}'");
        return None;
    }
    if rel.rel_types_alt.is_empty() {
        let safe = escape_sq(rt);
        return Some(Some(format!("{alias}.\"__type\" = '{safe}'")));
    }
    let mut all_types = vec![rt.clone()];
    all_types.extend(rel.rel_types_alt.iter().cloned());
    for t in &all_types {
        if validate_rel_type(t).is_err() {
            warn!("cypher translate: invalid relationship type '{t}'");
            return None;
        }
    }
    let in_list: Vec<String> = all_types
        .iter()
        .map(|t| format!("'{}'", escape_sq(t).to_uppercase()))
        .collect();
    Some(Some(format!(
        "{alias}.\"__type\" IN ({})",
        in_list.join(", ")
    )))
}

/// Build a type filter SQL expression for a variable-length edge alias.
/// Returns `None` if a relationship type contains unsafe characters.
/// Yields the literal `"true"` when the relationship has no declared type.
fn varlen_type_filter(
    rel: &aiondb_parser::cypher_ast::CypherRelPattern,
    alias: &str,
) -> Option<String> {
    rel_type_filter(rel, alias).map(|opt| opt.unwrap_or_else(|| "true".to_string()))
}

/// Build SELECT projections for RETURN items in a MATCH context.
fn build_match_select_items(
    ret: &CypherReturnClause,
    all_vars: &[String],
    rel_vars: &[String],
) -> Vec<String> {
    ret.items
        .iter()
        .map(|item| {
            let sql_expr = match_return_item_to_sql(&item.expr, all_vars, rel_vars);
            let alias = item
                .alias
                .as_deref()
                .map_or_else(|| cypher_return_alias(&item.expr), String::from);
            format!("{sql_expr} AS {}", qi(&alias))
        })
        .collect()
}

/// Build FROM clause from parts and join parts.
fn build_match_from_clause(from_parts: &[String], join_parts: &[String]) -> String {
    let mut sql = from_parts.join(", ");
    for join_part in join_parts {
        sql.push(' ');
        sql.push_str(join_part);
    }
    sql
}

/// Build WHERE clause from conditions.
fn build_match_where_clause(conditions: &[String]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    }
}

/// Append ORDER BY, SKIP, LIMIT to a SQL string.
fn apply_order_skip_limit(
    sql: &mut String,
    ret: &CypherReturnClause,
    all_vars: &[String],
    rel_vars: &[String],
) {
    if !ret.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        let items: Vec<String> = ret
            .order_by
            .iter()
            .map(|o| {
                let e = match_return_item_to_sql(&o.expr, all_vars, rel_vars);
                format!("{e}{}", order_by_suffix(o.descending, o.nulls_first))
            })
            .collect();
        sql.push_str(&items.join(", "));
    }
    append_skip_limit(sql, ret.skip.as_ref(), ret.limit.as_ref());
}

fn translate_match_return_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let CypherClause::Return(ret) = clauses.last()? else {
        warn!("cypher translate: MATCH pipeline missing final RETURN clause");
        return None;
    };
    let mut node_vars: Vec<String> = Vec::new();
    let mut rel_vars: Vec<String> = Vec::new();
    let mut from_parts: Vec<String> = Vec::new();
    let mut join_parts: Vec<String> = Vec::new();
    let mut conditions: Vec<String> = Vec::new();
    let mut anon_node_counter = 0usize;

    for clause in &clauses[..clauses.len() - 1] {
        match clause {
            CypherClause::Match(m) => {
                let join_kw = if m.optional { "LEFT JOIN" } else { "JOIN" };
                for pattern in &m.patterns {
                    if pattern.rels.is_empty() {
                        // Node-only pattern
                        for node in &pattern.nodes {
                            let var = match node.variable.as_deref() {
                                Some(v) => v.to_string(),
                                None => {
                                    let n = format!("__anon{anon_node_counter}");
                                    anon_node_counter += 1;
                                    n
                                }
                            };
                            if !node_vars.contains(&var) {
                                node_vars.push(var.clone());
                                let alias = qi(&var);
                                if m.optional && !from_parts.is_empty() {
                                    // OPTIONAL MATCH node-only: LEFT JOIN with label conds in ON
                                    let mut on_conds: Vec<String> = vec!["true".to_string()];
                                    for label in &node.labels {
                                        on_conds.push(label_check(&alias, label));
                                    }
                                    join_parts.push(format!(
                                        "LEFT JOIN \"{CYPHER_NODES_TABLE}\" AS {alias} ON {}",
                                        on_conds.join(" AND ")
                                    ));
                                } else {
                                    from_parts.push(format!("\"{CYPHER_NODES_TABLE}\" AS {alias}"));
                                    for label in &node.labels {
                                        conditions.push(label_check(&alias, label));
                                    }
                                }
                            }
                        }
                    } else {
                        let pattern_node_vars: Vec<String> = pattern
                            .nodes
                            .iter()
                            .map(|n| match n.variable.as_deref() {
                                Some(v) => v.to_string(),
                                None => {
                                    let name = format!("__anon{anon_node_counter}");
                                    anon_node_counter += 1;
                                    name
                                }
                            })
                            .collect();

                        for (i, node) in pattern.nodes.iter().enumerate() {
                            let var = &pattern_node_vars[i];
                            if node_vars.contains(var) {
                                if !m.optional {
                                    for label in &node.labels {
                                        conditions.push(label_check(&qi(var), label));
                                    }
                                }
                                continue;
                            }
                            node_vars.push(var.clone());
                            let alias = qi(var);
                            if i == 0 {
                                if m.optional && !from_parts.is_empty() {
                                    let mut on_conds: Vec<String> = vec!["true".to_string()];
                                    for label in &node.labels {
                                        on_conds.push(label_check(&alias, label));
                                    }
                                    join_parts.push(format!(
                                        "LEFT JOIN \"{CYPHER_NODES_TABLE}\" AS {alias} ON {}",
                                        on_conds.join(" AND ")
                                    ));
                                } else {
                                    from_parts.push(format!("\"{CYPHER_NODES_TABLE}\" AS {alias}"));
                                    for label in &node.labels {
                                        conditions.push(label_check(&alias, label));
                                    }
                                }
                            } else if !m.optional {
                                for label in &node.labels {
                                    conditions.push(label_check(&alias, label));
                                }
                            }
                            // For optional + i>0, label conditions go in the node JOIN ON clause below
                        }

                        for (i, rel) in pattern.rels.iter().enumerate() {
                            // -- Variable-length relationship --
                            if rel.variable_length {
                                let min = rel.min_hops.unwrap_or(1);
                                let max = rel
                                    .max_hops
                                    .unwrap_or(MAX_VAR_LENGTH_HOPS)
                                    .min(MAX_VAR_LENGTH_HOPS);
                                let left_var = &pattern_node_vars[i];
                                let right_var = &pattern_node_vars[i + 1];
                                let left_alias = qi(left_var);
                                let right_alias = qi(right_var);

                                let (src_col, tgt_col) = match rel.direction {
                                    CypherDirection::Outgoing => ("\"__source\"", "\"__target\""),
                                    CypherDirection::Incoming => ("\"__target\"", "\"__source\""),
                                    CypherDirection::Both => ("\"__source\"", "\"__target\""),
                                };
                                let both_dir = matches!(rel.direction, CypherDirection::Both);

                                let mut union_parts: Vec<String> = Vec::new();

                                // 0-hop case: identity (a = b)
                                if min == 0 {
                                    union_parts.push(format!(
                                        "SELECT \"__id\" AS __start, \"__id\" AS __end FROM \"{CYPHER_NODES_TABLE}\""
                                    ));
                                }

                                for depth in 1..=max {
                                    if depth < min {
                                        continue;
                                    }
                                    let mut from_clause = String::new();
                                    let mut where_parts: Vec<String> = Vec::new();

                                    for d in 1..=depth {
                                        let ea = format!("__e{d}");
                                        let tf = varlen_type_filter(rel, &ea)?;
                                        if d == 1 {
                                            from_clause =
                                                format!("\"{CYPHER_EDGES_TABLE}\" AS {ea}");
                                        } else {
                                            let prev_ea = format!("__e{}", d - 1);
                                            if both_dir {
                                                from_clause = format!(
                                                    "{from_clause} JOIN \"{CYPHER_EDGES_TABLE}\" AS {ea} ON \
                                                     ({ea}.{src_col} = {prev_ea}.{tgt_col} OR {ea}.{src_col} = {prev_ea}.{src_col})"
                                                );
                                            } else {
                                                from_clause = format!(
                                                    "{from_clause} JOIN \"{CYPHER_EDGES_TABLE}\" AS {ea} ON \
                                                     {ea}.{src_col} = {prev_ea}.{tgt_col}"
                                                );
                                            }
                                        }
                                        if tf != "true" {
                                            where_parts.push(tf);
                                        }
                                    }

                                    let first_ea = "__e1";
                                    let last_ea = format!("__e{depth}");
                                    let start_expr = format!("{first_ea}.{src_col}");
                                    let end_expr = format!("{last_ea}.{tgt_col}");

                                    let where_clause = if where_parts.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" WHERE {}", where_parts.join(" AND "))
                                    };

                                    let query = format!(
                                        "SELECT {start_expr} AS __start, {end_expr} AS __end FROM {from_clause}{where_clause}"
                                    );

                                    if both_dir && depth == 1 {
                                        let rev_start = format!("{first_ea}.{tgt_col}");
                                        let rev_end = format!("{last_ea}.{src_col}");
                                        let rev_query = format!(
                                            "SELECT {rev_start} AS __start, {rev_end} AS __end FROM {from_clause}{where_clause}"
                                        );
                                        union_parts.push(query);
                                        union_parts.push(rev_query);
                                    } else {
                                        union_parts.push(query);
                                    }
                                }

                                if union_parts.is_empty() {
                                    continue;
                                }

                                let cte_name = format!("__varlen_{}", rel_vars.len());
                                let cte_alias = qi(&cte_name);
                                let varlen_inner = union_parts.join(" UNION ");
                                let varlen_subquery = format!("({varlen_inner}) AS {cte_alias}");

                                let right_already_added = from_parts
                                    .iter()
                                    .any(|f| f.contains(&format!("AS {right_alias}")))
                                    || join_parts
                                        .iter()
                                        .any(|j| j.contains(&format!("AS {right_alias} ON")));

                                let varlen_on =
                                    format!("{cte_alias}.__start = {left_alias}.\"__id\"");
                                join_parts
                                    .push(format!("{join_kw} {varlen_subquery} ON {varlen_on}"));

                                if right_already_added {
                                    conditions.push(format!(
                                        "{right_alias}.\"__id\" = {cte_alias}.__end"
                                    ));
                                } else {
                                    let mut full_on =
                                        format!("{right_alias}.\"__id\" = {cte_alias}.__end");
                                    if m.optional {
                                        if let Some(node) = pattern.nodes.get(i + 1) {
                                            // Stream each AND-clause directly
                                            // instead of allocating a transient
                                            // format!() String per label.
                                            use std::fmt::Write;
                                            for label in &node.labels {
                                                let _ = write!(
                                                    full_on,
                                                    " AND {}",
                                                    label_check(&right_alias, label)
                                                );
                                            }
                                        }
                                    }
                                    join_parts.push(format!(
                                        "{join_kw} \"{CYPHER_NODES_TABLE}\" AS {right_alias} ON {full_on}"
                                    ));
                                }

                                if let Some(ref rv) = rel.variable {
                                    if !rel_vars.contains(rv) {
                                        rel_vars.push(rv.clone());
                                    }
                                }
                                continue;
                            }

                            // -- Fixed-length relationship --
                            let default_rel_name = format!("__rel{}", rel_vars.len());
                            let rel_var = rel.variable.as_deref().unwrap_or(&default_rel_name);
                            let rel_alias = qi(rel_var);
                            let left_var = &pattern_node_vars[i];
                            let right_var = &pattern_node_vars[i + 1];
                            let left_alias = qi(left_var);
                            let right_alias = qi(right_var);

                            let mut rel_conditions: Vec<String> = Vec::new();
                            if let Some(filter) = rel_type_filter(rel, &rel_alias)? {
                                rel_conditions.push(filter);
                            }

                            for (k, v) in &rel.properties {
                                let key = escape_sq(k);
                                let val = expr_to_sql_plain(v);
                                rel_conditions.push(format!(
                                    "{rel_alias}.\"__props\"->>'{key}' = ({val})::TEXT"
                                ));
                            }

                            let (src_on, tgt_on) = match rel.direction {
                                CypherDirection::Outgoing => {
                                    (
                                        format!("{rel_alias}.\"__source\" = {left_alias}.\"__id\""),
                                        format!("{rel_alias}.\"__target\" = {right_alias}.\"__id\""),
                                    )
                                }
                                CypherDirection::Incoming => {
                                    (
                                        format!("{rel_alias}.\"__target\" = {left_alias}.\"__id\""),
                                        format!("{rel_alias}.\"__source\" = {right_alias}.\"__id\""),
                                    )
                                }
                                CypherDirection::Both => {
                                    (
                                        format!("({rel_alias}.\"__source\" = {left_alias}.\"__id\" OR {rel_alias}.\"__target\" = {left_alias}.\"__id\")"),
                                        format!("({rel_alias}.\"__target\" = {right_alias}.\"__id\" OR {rel_alias}.\"__source\" = {right_alias}.\"__id\")"),
                                    )
                                }
                            };

                            let right_already_added_for_on = from_parts
                                .iter()
                                .any(|f| f.contains(&format!("AS {right_alias}")))
                                || join_parts
                                    .iter()
                                    .any(|j| j.contains(&format!("AS {right_alias} ON")));

                            let mut on_clause = if right_already_added_for_on {
                                format!("{src_on} AND {tgt_on}")
                            } else {
                                src_on.clone()
                            };
                            for rc in &rel_conditions {
                                on_clause.push_str(" AND ");
                                on_clause.push_str(rc);
                            }

                            join_parts.push(format!(
                                "{join_kw} \"{CYPHER_EDGES_TABLE}\" AS {rel_alias} ON {on_clause}"
                            ));

                            if !right_already_added_for_on {
                                let node_on = match rel.direction {
                                    CypherDirection::Outgoing => {
                                        format!("{right_alias}.\"__id\" = {rel_alias}.\"__target\"")
                                    }
                                    CypherDirection::Incoming => {
                                        format!("{right_alias}.\"__id\" = {rel_alias}.\"__source\"")
                                    }
                                    CypherDirection::Both => {
                                        format!("({right_alias}.\"__id\" = {rel_alias}.\"__target\" OR {right_alias}.\"__id\" = {rel_alias}.\"__source\")")
                                    }
                                };
                                let mut full_node_on = node_on;
                                if m.optional {
                                    if let Some(node) = pattern.nodes.get(i + 1) {
                                        // Stream each AND-clause directly
                                        // instead of `push_str(&format!(...))`.
                                        use std::fmt::Write;
                                        for label in &node.labels {
                                            let _ = write!(
                                                full_node_on,
                                                " AND {}",
                                                label_check(&right_alias, label)
                                            );
                                        }
                                    }
                                }
                                join_parts.push(format!(
                                    "{join_kw} \"{CYPHER_NODES_TABLE}\" AS {right_alias} ON {full_node_on}"
                                ));
                            }

                            if !rel_vars.contains(&rel_var.to_string()) {
                                rel_vars.push(rel_var.to_string());
                            }
                        }
                    }
                }
                if let Some(ref wh) = m.where_clause {
                    conditions.push(match_expr_to_sql(wh, &node_vars, &rel_vars));
                }
            }
            CypherClause::With(_) | CypherClause::Unwind(_) => {}
            _ => {
                warn!("cypher translate: unsupported clause in MATCH pipeline");
                return None;
            }
        }
    }
    if from_parts.is_empty() {
        warn!("cypher translate: MATCH pipeline produced no FROM parts");
        return None;
    }

    // Pre-MATCH WITH clauses
    {
        let mut seen_match = false;
        let mut pre_items: Vec<String> = Vec::new();
        for clause in &clauses[..clauses.len() - 1] {
            match clause {
                CypherClause::Match(_) => {
                    seen_match = true;
                }
                CypherClause::With(w) if !seen_match => {
                    for item in &w.items {
                        let e = expr_to_sql_plain(&item.expr);
                        let a = item
                            .alias
                            .as_deref()
                            .map_or_else(|| cypher_return_alias(&item.expr), String::from);
                        pre_items.push(format!("{e} AS {}", qi(&a)));
                    }
                }
                _ => {}
            }
        }
        if !pre_items.is_empty() {
            from_parts.insert(
                0,
                format!("(SELECT {}) AS __with_src", pre_items.join(", ")),
            );
        }
    }

    // Post-MATCH WITH clauses
    let with_clauses: Vec<&aiondb_parser::cypher_ast::CypherWithClause> = {
        let mut seen_match = false;
        clauses[..clauses.len() - 1]
            .iter()
            .filter_map(|c| match c {
                CypherClause::Match(_) => {
                    seen_match = true;
                    None
                }
                CypherClause::With(w) if seen_match => Some(w),
                _ => None,
            })
            .collect()
    };

    let all_vars: Vec<String> = node_vars.iter().chain(rel_vars.iter()).cloned().collect();

    if with_clauses.is_empty() {
        let select_items = build_match_select_items(ret, &all_vars, &rel_vars);
        let mut sql = String::from("SELECT ");
        if ret.distinct {
            sql.push_str("DISTINCT ");
        }
        sql.push_str(&select_items.join(", "));
        sql.push_str(" FROM ");
        sql.push_str(&build_match_from_clause(&from_parts, &join_parts));
        sql.push_str(&build_match_where_clause(&conditions));
        apply_order_skip_limit(&mut sql, ret, &all_vars, &rel_vars);
        return Some(sql);
    }

    // WITH clauses present
    let mut inner_sql = String::new();
    let mut current_match_vars = all_vars.clone();
    let mut current_rel_vars = rel_vars.clone();
    let mut is_first_with = true;

    for (wi, w) in with_clauses.iter().enumerate() {
        let projected_aliases: Vec<String> = w
            .items
            .iter()
            .map(|item| {
                item.alias.clone().unwrap_or_else(|| {
                    if let Expr::Identifier(name) = &item.expr {
                        name.parts.last().cloned().unwrap_or_default()
                    } else {
                        cypher_return_alias(&item.expr)
                    }
                })
            })
            .collect();

        if is_first_with {
            let with_items: Vec<String> = w
                .items
                .iter()
                .map(|item| {
                    let sql_expr = match_return_item_to_sql(
                        &item.expr,
                        &current_match_vars,
                        &current_rel_vars,
                    );
                    let alias = item
                        .alias
                        .as_deref()
                        .map_or_else(|| cypher_return_alias(&item.expr), String::from);
                    format!("{sql_expr} AS {}", qi(&alias))
                })
                .collect();

            inner_sql = String::from("SELECT ");
            if w.distinct {
                inner_sql.push_str("DISTINCT ");
            }
            inner_sql.push_str(&with_items.join(", "));
            inner_sql.push_str(" FROM ");
            inner_sql.push_str(&from_parts.join(", "));
            for jp in &join_parts {
                inner_sql.push(' ');
                inner_sql.push_str(jp);
            }
            if !conditions.is_empty() {
                inner_sql.push_str(" WHERE ");
                inner_sql.push_str(&conditions.join(" AND "));
            }
            if !w.order_by.is_empty() {
                inner_sql.push_str(" ORDER BY ");
                let items: Vec<String> = w
                    .order_by
                    .iter()
                    .map(|o| {
                        let e = match_return_item_to_sql(
                            &o.expr,
                            &current_match_vars,
                            &current_rel_vars,
                        );
                        format!("{e}{}", order_by_suffix(o.descending, o.nulls_first))
                    })
                    .collect();
                inner_sql.push_str(&items.join(", "));
            }
            append_skip_limit(&mut inner_sql, w.skip.as_ref(), w.limit.as_ref());

            if let Some(ref wh) = w.where_clause {
                let prev = inner_sql;
                inner_sql = format!(
                    "SELECT * FROM ({}) AS __with_src WHERE {}",
                    prev,
                    match_expr_to_sql(wh, &current_match_vars, &current_rel_vars)
                );
            }

            is_first_with = false;
        } else {
            let sub_alias = format!("__with_{wi}");
            let with_items: Vec<String> = w
                .items
                .iter()
                .map(|item| {
                    let alias = item.alias.as_deref().map_or_else(
                        || {
                            if let Expr::Identifier(name) = &item.expr {
                                name.parts.last().cloned().unwrap_or_default()
                            } else {
                                cypher_return_alias(&item.expr)
                            }
                        },
                        String::from,
                    );
                    let sql_expr = expr_to_sql_plain(&item.expr);
                    format!("{sql_expr} AS {}", qi(&alias))
                })
                .collect();

            let mut new_sql = String::from("SELECT ");
            if w.distinct {
                new_sql.push_str("DISTINCT ");
            }
            new_sql.push_str(&with_items.join(", "));
            // Stream the FROM-subquery clause directly into `new_sql`.
            use std::fmt::Write;
            let _ = write!(new_sql, " FROM ({inner_sql}) AS \"{sub_alias}\"");
            append_order_by(&mut new_sql, &w.order_by);
            append_skip_limit(&mut new_sql, w.skip.as_ref(), w.limit.as_ref());

            if let Some(ref wh) = w.where_clause {
                let prev = new_sql;
                new_sql = format!(
                    "SELECT * FROM ({}) AS __with_src WHERE {}",
                    prev,
                    expr_to_sql_plain(wh)
                );
            }

            inner_sql = new_sql;
        }

        current_match_vars = projected_aliases;
        current_rel_vars = Vec::new();
    }

    let ret_items: Vec<String> = ret
        .items
        .iter()
        .map(|item| {
            let alias = item
                .alias
                .as_deref()
                .map_or_else(|| cypher_return_alias(&item.expr), String::from);
            qi(&alias)
        })
        .collect();

    let mut sql = String::from("SELECT ");
    if ret.distinct {
        sql.push_str("DISTINCT ");
    }
    sql.push_str(&ret_items.join(", "));
    sql.push_str(" FROM (");
    sql.push_str(&inner_sql);
    sql.push_str(") AS __cypher_ret");

    if !ret.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        let items: Vec<String> = ret
            .order_by
            .iter()
            .map(|o| {
                let alias = cypher_return_alias(&o.expr);
                let e = qi(&alias);
                format!("{e}{}", order_by_suffix(o.descending, o.nulls_first))
            })
            .collect();
        sql.push_str(&items.join(", "));
    }
    append_skip_limit(&mut sql, ret.skip.as_ref(), ret.limit.as_ref());
    Some(sql)
}
