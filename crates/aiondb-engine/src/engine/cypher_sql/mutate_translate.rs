//! SET, DELETE, REMOVE, MERGE clause translation.

#![allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

use aiondb_parser::ast::{Expr, Literal};
use aiondb_parser::cypher_ast::{CypherClause, CypherDirection, CypherRemoveItem, CypherSetItem};
use tracing::warn;

use super::escape::{escape_json_key, escape_sq, qi, validate_rel_type};
use super::expr::{
    append_skip_limit, cypher_expr_to_json_value, cypher_return_alias, expr_to_sql_plain,
    match_expr_to_sql, match_return_item_to_sql, order_by_suffix,
};
use super::{cypher_nodes_ddl, MatchContext, CYPHER_EDGES_TABLE, CYPHER_NODES_TABLE};

pub(crate) fn build_match_context(clauses: &[CypherClause]) -> Option<MatchContext> {
    let mut ctx = MatchContext::new();
    for clause in clauses {
        if let CypherClause::Match(m) = clause {
            for pat in &m.patterns {
                if pat.rels.is_empty() {
                    for node in &pat.nodes {
                        let var = node.variable.as_deref().unwrap_or("__anon");
                        if !ctx.node_vars.contains(&var.to_string()) {
                            ctx.node_vars.push(var.to_string());
                            let alias_var = qi(var);
                            ctx.from_parts
                                .push(format!("\"{CYPHER_NODES_TABLE}\" AS {alias_var}"));
                            for label in &node.labels {
                                let safe = escape_sq(label);
                                ctx.where_conditions
                                    .push(format!("'{safe}' = ANY({alias_var}.\"__labels\")"));
                            }
                            for (k, v) in &node.properties {
                                let key_safe = escape_sq(k);
                                let val_sql = expr_to_sql_plain(v);
                                ctx.where_conditions.push(format!(
                                    "{alias_var}.\"__props\"->>'{key_safe}' = ({val_sql})::TEXT"
                                ));
                            }
                        }
                    }
                } else {
                    for (i, node) in pat.nodes.iter().enumerate() {
                        let var = node.variable.as_deref().unwrap_or("__anon");
                        if ctx.node_vars.contains(&var.to_string()) {
                            for label in &node.labels {
                                let safe = escape_sq(label);
                                ctx.where_conditions
                                    .push(format!("'{safe}' = ANY({}.\"__labels\")", qi(var)));
                            }
                            continue;
                        }
                        ctx.node_vars.push(var.to_string());
                        let alias_var = qi(var);
                        if i == 0 {
                            ctx.from_parts
                                .push(format!("\"{CYPHER_NODES_TABLE}\" AS {alias_var}"));
                        }
                        for label in &node.labels {
                            let safe = escape_sq(label);
                            ctx.where_conditions
                                .push(format!("'{safe}' = ANY({alias_var}.\"__labels\")"));
                        }
                        for (k, v) in &node.properties {
                            let key_safe = escape_sq(k);
                            let val_sql = expr_to_sql_plain(v);
                            ctx.where_conditions.push(format!(
                                "{alias_var}.\"__props\"->>'{key_safe}' = ({val_sql})::TEXT"
                            ));
                        }
                    }
                    for (i, rel) in pat.rels.iter().enumerate() {
                        let rel_name = rel.variable.as_deref().unwrap_or("__anon_rel");
                        let rel_var = if rel_name == "__anon_rel" {
                            format!("__rel{}", ctx.rel_vars.len())
                        } else {
                            rel_name.to_string()
                        };
                        let rel_alias = qi(&rel_var);
                        let left_var = pat.nodes[i].variable.as_deref().unwrap_or("__anon");
                        let right_var_name =
                            pat.nodes[i + 1].variable.as_deref().unwrap_or("__anon");
                        let left_alias = qi(left_var);
                        let right_alias = qi(right_var_name);
                        let mut rel_conditions: Vec<String> = Vec::new();
                        if let Some(ref rel_type) = rel.rel_type {
                            if validate_rel_type(rel_type).is_err() {
                                warn!("cypher translate: invalid relationship type '{rel_type}'");
                                return None;
                            }
                            if rel.rel_types_alt.is_empty() {
                                let safe = escape_sq(rel_type);
                                rel_conditions.push(format!("{rel_alias}.\"__type\" = '{safe}'"));
                            } else {
                                let mut all_types = vec![rel_type.clone()];
                                all_types.extend(rel.rel_types_alt.iter().cloned());
                                for t in &all_types {
                                    if validate_rel_type(t).is_err() {
                                        warn!("cypher translate: invalid relationship type '{t}'");
                                        return None;
                                    }
                                }
                                let in_list: Vec<String> = all_types
                                    .iter()
                                    .map(|t| format!("'{}'", escape_sq(t)))
                                    .collect();
                                rel_conditions.push(format!(
                                    "{rel_alias}.\"__type\" IN ({})",
                                    in_list.join(", ")
                                ));
                            }
                        }
                        for (k, v) in &rel.properties {
                            let key_safe = escape_sq(k);
                            let val_sql = expr_to_sql_plain(v);
                            rel_conditions.push(format!(
                                "{rel_alias}.\"__props\"->>'{key_safe}' = ({val_sql})::TEXT"
                            ));
                        }
                        let (src_on, tgt_on) = match rel.direction {
                            CypherDirection::Outgoing => (
                                format!("{rel_alias}.\"__source\" = {left_alias}.\"__id\""),
                                format!("{rel_alias}.\"__target\" = {right_alias}.\"__id\""),
                            ),
                            CypherDirection::Incoming => (
                                format!("{rel_alias}.\"__target\" = {left_alias}.\"__id\""),
                                format!("{rel_alias}.\"__source\" = {right_alias}.\"__id\""),
                            ),
                            CypherDirection::Both => (
                                format!("({rel_alias}.\"__source\" = {left_alias}.\"__id\" OR {rel_alias}.\"__target\" = {left_alias}.\"__id\")"),
                                format!("({rel_alias}.\"__target\" = {right_alias}.\"__id\" OR {rel_alias}.\"__source\" = {right_alias}.\"__id\")"),
                            ),
                        };
                        let already_added = ctx
                            .from_parts
                            .iter()
                            .any(|f: &String| f.contains(&format!("AS {right_alias}")))
                            || ctx
                                .join_parts
                                .iter()
                                .any(|j: &String| j.contains(&format!("AS {right_alias} ON")));
                        let mut on_clause = if already_added {
                            format!("{src_on} AND {tgt_on}")
                        } else {
                            src_on.clone()
                        };
                        for cond in &rel_conditions {
                            on_clause.push_str(" AND ");
                            on_clause.push_str(cond);
                        }
                        ctx.join_parts.push(format!(
                            "JOIN \"{CYPHER_EDGES_TABLE}\" AS {rel_alias} ON {on_clause}"
                        ));
                        if !already_added {
                            let node_on = match rel.direction {
                                CypherDirection::Outgoing => format!("{right_alias}.\"__id\" = {rel_alias}.\"__target\""),
                                CypherDirection::Incoming => format!("{right_alias}.\"__id\" = {rel_alias}.\"__source\""),
                                CypherDirection::Both => format!("({right_alias}.\"__id\" = {rel_alias}.\"__target\" OR {right_alias}.\"__id\" = {rel_alias}.\"__source\")"),
                            };
                            ctx.join_parts.push(format!(
                                "JOIN \"{CYPHER_NODES_TABLE}\" AS {right_alias} ON {node_on}"
                            ));
                        }
                        if !ctx.rel_vars.contains(&rel_var) {
                            ctx.rel_vars.push(rel_var);
                        }
                    }
                }
            }
            if let Some(ref wh) = m.where_clause {
                let all_vars = ctx.all_vars();
                ctx.where_conditions
                    .push(match_expr_to_sql(wh, &all_vars, &ctx.rel_vars));
            }
        }
    }
    if ctx.from_parts.is_empty() {
        warn!("cypher translate: build_match_context produced no FROM parts");
        return None;
    }
    Some(ctx)
}

fn build_from_where(ctx: &MatchContext) -> String {
    ctx.from_where_sql()
}

fn build_update_stmt(alias: &str, set_expr: &str, ctx: &MatchContext) -> String {
    build_update_stmt_table(alias, set_expr, CYPHER_NODES_TABLE, ctx)
}

fn build_update_stmt_table(alias: &str, set_expr: &str, table: &str, ctx: &MatchContext) -> String {
    if ctx.from_parts.len() == 1 && ctx.join_parts.is_empty() {
        let where_clause = if ctx.where_conditions.is_empty() {
            String::new()
        } else {
            let conditions: Vec<String> = ctx
                .where_conditions
                .iter()
                .map(|c: &String| c.replace(&format!("{alias}."), ""))
                .collect();
            format!(" WHERE {}", conditions.join(" AND "))
        };
        format!("UPDATE \"{table}\" SET {set_expr}{where_clause}")
    } else {
        let inner = format!("SELECT {alias}.\"__id\" FROM {}", build_from_where(ctx));
        format!("UPDATE \"{table}\" SET {set_expr} WHERE \"__id\" IN ({inner})")
    }
}

pub(crate) fn build_match_select(
    ret: &aiondb_parser::cypher_ast::CypherReturnClause,
    ctx: &MatchContext,
) -> String {
    let all_vars = ctx.all_vars();
    let select_items: Vec<String> = ret
        .items
        .iter()
        .map(|item| {
            let expr_sql = match_return_item_to_sql(&item.expr, &all_vars, &ctx.rel_vars);
            let alias = item
                .alias
                .as_deref()
                .map_or_else(|| cypher_return_alias(&item.expr), String::from);
            format!("{expr_sql} AS {}", qi(&alias))
        })
        .collect();
    let mut sql = String::from("SELECT ");
    if ret.distinct {
        sql.push_str("DISTINCT ");
    }
    sql.push_str(&select_items.join(", "));
    sql.push_str(" FROM ");
    sql.push_str(&build_from_where(ctx));
    if !ret.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        let order_items: Vec<String> = ret
            .order_by
            .iter()
            .map(|o| {
                let expr_sql = match_return_item_to_sql(&o.expr, &all_vars, &ctx.rel_vars);
                format!("{expr_sql}{}", order_by_suffix(o.descending, o.nulls_first))
            })
            .collect();
        sql.push_str(&order_items.join(", "));
    }
    append_skip_limit(&mut sql, ret.skip.as_ref(), ret.limit.as_ref());
    sql
}

fn translate_one_set_item(item: &CypherSetItem, ctx: &MatchContext) -> Option<String> {
    let all_vars = ctx.all_vars();
    match item {
        CypherSetItem::Property {
            variable,
            property,
            expr,
            ..
        } => {
            let val_sql = match_return_item_to_sql(expr, &all_vars, &ctx.rel_vars);
            let key_safe = escape_json_key(property);
            let alias_var = qi(variable);
            let is_rel = ctx.rel_vars.contains(variable);
            let table = if is_rel {
                CYPHER_EDGES_TABLE
            } else {
                CYPHER_NODES_TABLE
            };
            let set_expr = if matches!(expr, Expr::Literal(Literal::Null, _)) {
                format!("\"__props\" = jsonb_delete(\"__props\", '{key_safe}')")
            } else {
                format!(
                    "\"__props\" = jsonb_set(\"__props\", '{{{key_safe}}}', to_jsonb({val_sql}))"
                )
            };
            Some(build_update_stmt_table(&alias_var, &set_expr, table, ctx))
        }
        CypherSetItem::Label {
            variable, label, ..
        } => {
            let alias_var = qi(variable);
            let safe = escape_sq(label);
            let set_expr = format!("\"__labels\" = CASE WHEN '{safe}' = ANY(\"__labels\") THEN \"__labels\" ELSE array_append(\"__labels\", '{safe}') END");
            Some(build_update_stmt(&alias_var, &set_expr, ctx))
        }
        CypherSetItem::ReplaceProperties {
            variable, entries, ..
        } => {
            let alias_var = qi(variable);
            let is_rel = ctx.rel_vars.contains(variable);
            let table = if is_rel {
                CYPHER_EDGES_TABLE
            } else {
                CYPHER_NODES_TABLE
            };
            let json_entries: Vec<String> = entries
                .iter()
                .filter(|(_, v)| !matches!(v.as_ref(), Expr::Literal(Literal::Null, _)))
                .map(|(k, v)| {
                    let key = escape_json_key(k);
                    format!("\"{key}\": {}", cypher_expr_to_json_value(v))
                })
                .collect();
            let new_props = format!("'{{{}}}'::JSONB", json_entries.join(", "));
            Some(build_update_stmt_table(
                &alias_var,
                &format!("\"__props\" = {new_props}"),
                table,
                ctx,
            ))
        }
        CypherSetItem::MergeProperties {
            variable, entries, ..
        } => {
            let alias_var = qi(variable);
            let is_rel = ctx.rel_vars.contains(variable);
            let table = if is_rel {
                CYPHER_EDGES_TABLE
            } else {
                CYPHER_NODES_TABLE
            };
            let non_null_entries: Vec<String> = entries
                .iter()
                .filter(|(_, v)| !matches!(v.as_ref(), Expr::Literal(Literal::Null, _)))
                .map(|(k, v)| {
                    let key = escape_json_key(k);
                    format!("\"{key}\": {}", cypher_expr_to_json_value(v))
                })
                .collect();
            let null_keys: Vec<&String> = entries
                .iter()
                .filter(|(_, v)| matches!(v.as_ref(), Expr::Literal(Literal::Null, _)))
                .map(|(k, _)| k)
                .collect();
            let mut expr_str = format!(
                "\"__props\" || '{{{}}}'::JSONB",
                non_null_entries.join(", ")
            );
            for null_key in &null_keys {
                let key_safe = escape_json_key(null_key);
                expr_str = format!("jsonb_delete({expr_str}, '{key_safe}')");
            }
            Some(build_update_stmt_table(
                &alias_var,
                &format!("\"__props\" = {expr_str}"),
                table,
                ctx,
            ))
        }
    }
}

pub(crate) fn translate_match_set_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let ctx = build_match_context(clauses)?;
    let set_clauses: Vec<&aiondb_parser::cypher_ast::CypherSetClause> = clauses
        .iter()
        .filter_map(|c| {
            if let CypherClause::Set(s) = c {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    let ret = clauses.iter().rev().find_map(|c| {
        if let CypherClause::Return(r) = c {
            Some(r)
        } else {
            None
        }
    });
    let mut stmts: Vec<String> = Vec::new();
    for set_clause in &set_clauses {
        for item in &set_clause.items {
            if let Some(u) = translate_one_set_item(item, &ctx) {
                stmts.push(u);
            }
        }
    }
    if let Some(ret) = ret {
        let labels_ctx = build_match_context_labels_only(clauses).unwrap_or(ctx);
        stmts.push(build_match_select(ret, &labels_ctx));
    }
    if stmts.is_empty() {
        warn!("cypher translate: MATCH SET pipeline produced no statements");
        return None;
    }
    Some(stmts.join("; "))
}

pub(crate) fn translate_match_delete_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let ctx = build_match_context(clauses)?;
    let delete_clauses: Vec<&aiondb_parser::cypher_ast::CypherDeleteClause> = clauses
        .iter()
        .filter_map(|c| {
            if let CypherClause::Delete(d) = c {
                Some(d)
            } else {
                None
            }
        })
        .collect();
    let mut stmts: Vec<String> = Vec::new();
    for dc in &delete_clauses {
        for var in &dc.variables {
            let alias_var = qi(var);
            if ctx.from_parts.len() == 1 && ctx.join_parts.is_empty() {
                let where_clause = if ctx.where_conditions.is_empty() {
                    String::new()
                } else {
                    let conditions: Vec<String> = ctx
                        .where_conditions
                        .iter()
                        .map(|c: &String| c.replace(&format!("{alias_var}."), ""))
                        .collect();
                    format!(" WHERE {}", conditions.join(" AND "))
                };
                if dc.detach {
                    stmts.push(format!("DELETE FROM \"{CYPHER_EDGES_TABLE}\" WHERE \"__source\" IN (SELECT \"__id\" FROM \"{CYPHER_NODES_TABLE}\"{where_clause}) OR \"__target\" IN (SELECT \"__id\" FROM \"{CYPHER_NODES_TABLE}\"{where_clause})"));
                }
                stmts.push(format!(
                    "DELETE FROM \"{CYPHER_NODES_TABLE}\"{where_clause}"
                ));
            } else {
                let inner = format!(
                    "SELECT {alias_var}.\"__id\" FROM {}",
                    build_from_where(&ctx)
                );
                if dc.detach {
                    stmts.push(format!("DELETE FROM \"{CYPHER_EDGES_TABLE}\" WHERE \"__source\" IN ({inner}) OR \"__target\" IN ({inner})"));
                }
                stmts.push(format!(
                    "DELETE FROM \"{CYPHER_NODES_TABLE}\" WHERE \"__id\" IN ({inner})"
                ));
            }
        }
    }
    let ret = clauses.iter().rev().find_map(|c| {
        if let CypherClause::Return(r) = c {
            Some(r)
        } else {
            None
        }
    });
    if let Some(ret) = ret {
        stmts.push(build_match_select(ret, &ctx));
    }
    if stmts.is_empty() {
        warn!("cypher translate: MATCH DELETE pipeline produced no statements");
        return None;
    }
    Some(stmts.join("; "))
}

pub(crate) fn translate_match_remove_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let ctx = build_match_context(clauses)?;
    let remove_clauses: Vec<&aiondb_parser::cypher_ast::CypherRemoveClause> = clauses
        .iter()
        .filter_map(|c| {
            if let CypherClause::Remove(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .collect();
    let mut stmts: Vec<String> = Vec::new();
    for remove_clause in &remove_clauses {
        for item in &remove_clause.items {
            match item {
                CypherRemoveItem::Property {
                    variable, property, ..
                } => {
                    let alias_var = qi(variable);
                    let key_safe = escape_json_key(property);
                    let is_rel = ctx.rel_vars.contains(variable);
                    let table = if is_rel {
                        CYPHER_EDGES_TABLE
                    } else {
                        CYPHER_NODES_TABLE
                    };
                    stmts.push(build_update_stmt_table(
                        &alias_var,
                        &format!("\"__props\" = jsonb_delete(\"__props\", '{key_safe}')"),
                        table,
                        &ctx,
                    ));
                }
                CypherRemoveItem::Label {
                    variable, label, ..
                } => {
                    let alias_var = qi(variable);
                    let safe = escape_sq(label);
                    stmts.push(build_update_stmt(
                        &alias_var,
                        &format!("\"__labels\" = array_remove(\"__labels\", '{safe}')"),
                        &ctx,
                    ));
                }
            }
        }
    }
    let ret = clauses.iter().rev().find_map(|c| {
        if let CypherClause::Return(r) = c {
            Some(r)
        } else {
            None
        }
    });
    if let Some(ret) = ret {
        let labels_ctx = build_match_context_labels_only(clauses).unwrap_or(ctx);
        stmts.push(build_match_select(ret, &labels_ctx));
    }
    if stmts.is_empty() {
        warn!("cypher translate: MATCH REMOVE pipeline produced no statements");
        return None;
    }
    Some(stmts.join("; "))
}

pub(crate) fn translate_merge_pipeline(clauses: &[CypherClause]) -> Option<String> {
    translate_merge_pipeline_impl(clauses)
}

fn translate_merge_pipeline_impl(clauses: &[CypherClause]) -> Option<String> {
    let mut stmts: Vec<String> = Vec::new();
    stmts.push(cypher_nodes_ddl());
    let mut last_var = String::new();
    let mut label_list: Vec<String> = Vec::new();
    let mut label_props: Vec<(String, Expr)> = Vec::new();
    for clause in clauses {
        if let CypherClause::Merge(m) = clause {
            if !m.pattern.rels.is_empty() || m.pattern.nodes.is_empty() {
                warn!("cypher translate: MERGE with relationships or empty nodes is unsupported");
                return None;
            }
            let node = &m.pattern.nodes[0];
            let var = node.variable.as_deref().unwrap_or("__anon").to_string();
            let mut match_conditions: Vec<String> = Vec::new();
            for label in &node.labels {
                let safe = escape_sq(label);
                match_conditions.push(format!("'{safe}' = ANY(\"__labels\")"));
            }
            for (k, v) in &node.properties {
                let key_safe = escape_sq(k);
                let val_sql = expr_to_sql_plain(v);
                match_conditions.push(format!("\"__props\"->>'{key_safe}' = ({val_sql})::TEXT"));
            }
            let labels_sql = if node.labels.is_empty() {
                "ARRAY[]::TEXT[]".into()
            } else {
                let entries: Vec<String> = node
                    .labels
                    .iter()
                    .map(|l| format!("'{}'", escape_sq(l)))
                    .collect();
                format!("ARRAY[{}]::TEXT[]", entries.join(", "))
            };
            let props_json = if node.properties.is_empty() {
                "'{}'::JSONB".into()
            } else {
                let entries: Vec<String> = node
                    .properties
                    .iter()
                    .map(|(k, v)| {
                        let key = escape_json_key(k);
                        format!("\"{key}\": {}", cypher_expr_to_json_value(v))
                    })
                    .collect();
                format!("'{{{}}}'::JSONB", entries.join(", "))
            };
            let insert_sql = if match_conditions.is_empty() {
                format!("INSERT INTO \"{CYPHER_NODES_TABLE}\" (\"__labels\", \"__props\") SELECT {labels_sql}, {props_json} WHERE NOT EXISTS (SELECT 1 FROM \"{CYPHER_NODES_TABLE}\")")
            } else {
                let cond_str = match_conditions.join(" AND ");
                format!("INSERT INTO \"{CYPHER_NODES_TABLE}\" (\"__labels\", \"__props\") SELECT {labels_sql}, {props_json} WHERE NOT EXISTS (SELECT 1 FROM \"{CYPHER_NODES_TABLE}\" WHERE {cond_str})")
            };
            stmts.push(insert_sql);
            for action in &m.actions {
                let cond_where = if match_conditions.is_empty() {
                    String::new()
                } else {
                    format!(" WHERE {}", match_conditions.join(" AND "))
                };
                for set_item in &action.items {
                    if let Some(u) = merge_action_set_sql(set_item, &cond_where, action.on_create) {
                        stmts.push(u);
                    }
                }
            }
            last_var = var;
            node.labels.clone_into(&mut label_list);
            label_props = node
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
    }
    let ret = clauses.iter().rev().find_map(|c| {
        if let CypherClause::Return(r) = c {
            Some(r)
        } else {
            None
        }
    });
    if let Some(ret) = ret {
        let var = if last_var.is_empty() {
            "__anon"
        } else {
            &last_var
        };
        let node_var_list = vec![var.to_string()];
        let alias_var = qi(var);
        let mut match_conditions: Vec<String> = Vec::new();
        for label in &label_list {
            let safe = escape_sq(label);
            match_conditions.push(format!("'{safe}' = ANY({alias_var}.\"__labels\")"));
        }
        for (k, v) in &label_props {
            let key_safe = escape_sq(k);
            let val_sql = expr_to_sql_plain(v);
            match_conditions.push(format!(
                "{alias_var}.\"__props\"->>'{key_safe}' = ({val_sql})::TEXT"
            ));
        }
        let select_items: Vec<String> = ret
            .items
            .iter()
            .map(|item| {
                let expr_sql = match_return_item_to_sql(&item.expr, &node_var_list, &[]);
                let alias = item
                    .alias
                    .as_deref()
                    .map_or_else(|| cypher_return_alias(&item.expr), String::from);
                format!("{expr_sql} AS {}", qi(&alias))
            })
            .collect();
        let mut sel = String::from("SELECT ");
        if ret.distinct {
            sel.push_str("DISTINCT ");
        }
        sel.push_str(&select_items.join(", "));
        // Stream the FROM clause directly instead of allocating a
        // transient `format!` String just to push_str it.
        use std::fmt::Write;
        let _ = write!(sel, " FROM \"{CYPHER_NODES_TABLE}\" AS {alias_var}");
        if !match_conditions.is_empty() {
            sel.push_str(" WHERE ");
            sel.push_str(&match_conditions.join(" AND "));
        }
        if !ret.order_by.is_empty() {
            sel.push_str(" ORDER BY ");
            let order_items: Vec<String> = ret
                .order_by
                .iter()
                .map(|o| {
                    let expr_sql = match_return_item_to_sql(&o.expr, &node_var_list, &[]);
                    let dir = if o.descending { " DESC" } else { "" };
                    let nulls = match o.nulls_first {
                        Some(true) => " NULLS FIRST",
                        Some(false) => " NULLS LAST",
                        None => "",
                    };
                    format!("{expr_sql}{dir}{nulls}")
                })
                .collect();
            sel.push_str(&order_items.join(", "));
        }
        append_skip_limit(&mut sel, ret.skip.as_ref(), ret.limit.as_ref());
        stmts.push(sel);
    }
    if stmts.len() <= 1 {
        warn!("cypher translate: MERGE pipeline produced only DDL, no actual work");
        return None;
    }
    Some(stmts.join("; "))
}

fn merge_action_set_sql(
    item: &CypherSetItem,
    cond_where: &str,
    _on_create: bool,
) -> Option<String> {
    match item {
        CypherSetItem::Property { property, expr, .. } => {
            let val_sql = expr_to_sql_plain(expr);
            let key_safe = escape_json_key(property);
            let set_expr = if matches!(expr, Expr::Literal(Literal::Null, _)) {
                format!("\"__props\" = jsonb_delete(\"__props\", '{key_safe}')")
            } else {
                format!(
                    "\"__props\" = jsonb_set(\"__props\", '{{{key_safe}}}', to_jsonb({val_sql}))"
                )
            };
            Some(format!(
                "UPDATE \"{CYPHER_NODES_TABLE}\" SET {set_expr}{cond_where}"
            ))
        }
        CypherSetItem::Label { label, .. } => {
            let safe = escape_sq(label);
            let set_expr = format!("\"__labels\" = CASE WHEN '{safe}' = ANY(\"__labels\") THEN \"__labels\" ELSE array_append(\"__labels\", '{safe}') END");
            Some(format!(
                "UPDATE \"{CYPHER_NODES_TABLE}\" SET {set_expr}{cond_where}"
            ))
        }
        CypherSetItem::ReplaceProperties { entries, .. } => {
            let json_entries: Vec<String> = entries
                .iter()
                .filter(|(_, v)| !matches!(v.as_ref(), Expr::Literal(Literal::Null, _)))
                .map(|(k, v)| {
                    let key = escape_json_key(k);
                    format!("\"{key}\": {}", cypher_expr_to_json_value(v))
                })
                .collect();
            let new_props = format!("'{{{}}}'::JSONB", json_entries.join(", "));
            Some(format!(
                "UPDATE \"{CYPHER_NODES_TABLE}\" SET \"__props\" = {new_props}{cond_where}"
            ))
        }
        CypherSetItem::MergeProperties { entries, .. } => {
            let non_null_entries: Vec<String> = entries
                .iter()
                .filter(|(_, v)| !matches!(v.as_ref(), Expr::Literal(Literal::Null, _)))
                .map(|(k, v)| {
                    let key = escape_json_key(k);
                    format!("\"{key}\": {}", cypher_expr_to_json_value(v))
                })
                .collect();
            let merge_json = format!("'{{{}}}'::JSONB", non_null_entries.join(", "));
            Some(format!("UPDATE \"{CYPHER_NODES_TABLE}\" SET \"__props\" = \"__props\" || {merge_json}{cond_where}"))
        }
    }
}

fn build_match_context_labels_only(clauses: &[CypherClause]) -> Option<MatchContext> {
    let mut ctx = MatchContext::new();
    for clause in clauses {
        if let CypherClause::Match(m) = clause {
            for pat in &m.patterns {
                for node in &pat.nodes {
                    let var = node.variable.as_deref().unwrap_or("__anon");
                    if !ctx.node_vars.contains(&var.to_string()) {
                        ctx.node_vars.push(var.to_string());
                        let alias_var = qi(var);
                        ctx.from_parts
                            .push(format!("\"{CYPHER_NODES_TABLE}\" AS {alias_var}"));
                        for label in &node.labels {
                            let safe = escape_sq(label);
                            ctx.where_conditions
                                .push(format!("'{safe}' = ANY({alias_var}.\"__labels\")"));
                        }
                    }
                }
            }
        }
    }
    if ctx.from_parts.is_empty() {
        warn!("cypher translate: build_match_context_labels_only produced no FROM parts");
        return None;
    }
    Some(ctx)
}
