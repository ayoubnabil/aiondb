#![allow(clippy::pedantic)]

//! CREATE clause translation.

use aiondb_parser::ast::Expr;
use aiondb_parser::cypher_ast::{
    CypherClause, CypherCreateClause, CypherDirection, CypherNodePattern, CypherRelPattern,
};

use super::escape::{escape_json_key, escape_sq, qi, validate_rel_type};
use super::expr::{
    append_skip_limit, cypher_expr_to_json_value, cypher_return_alias, expr_to_sql_plain,
};
use super::{cypher_edges_ddl, cypher_nodes_ddl, CYPHER_EDGES_TABLE, CYPHER_NODES_TABLE};

pub(crate) fn node_labels_sql(node: &CypherNodePattern) -> String {
    if node.labels.is_empty() {
        "ARRAY[]::TEXT[]".into()
    } else {
        let e: Vec<String> = node
            .labels
            .iter()
            .map(|l| format!("'{}'", escape_sq(l)))
            .collect();
        format!("ARRAY[{}]::TEXT[]", e.join(", "))
    }
}

/// Render a Cypher property bag (`Vec<(String, Expr)>`) as a JSONB literal.
/// Used by both node and relationship pattern translation: shared helper
/// because the two property lists have identical shape and serialization.
fn props_to_jsonb_sql(properties: &[(String, Expr)]) -> String {
    if properties.is_empty() {
        return "'{}'::JSONB".into();
    }
    let e: Vec<String> = properties
        .iter()
        .map(|(k, v)| {
            let key = escape_json_key(k);
            let val = cypher_expr_to_json_value(v);
            format!("\"{key}\": {val}")
        })
        .collect();
    format!("'{{{}}}'::JSONB", e.join(", "))
}

pub(crate) fn node_props_sql(node: &CypherNodePattern) -> String {
    props_to_jsonb_sql(&node.properties)
}

pub(crate) fn rel_props_json(rel: &CypherRelPattern) -> String {
    props_to_jsonb_sql(&rel.properties)
}

pub(crate) fn translate_create_only(clauses: &[CypherClause]) -> Option<String> {
    let mut cc: Vec<&CypherCreateClause> = Vec::new();
    for clause in clauses {
        match clause {
            CypherClause::Create(c) => cc.push(c),
            _ => return None,
        }
    }
    if cc.is_empty() {
        return None;
    }
    let has_rels = cc
        .iter()
        .any(|c| c.patterns.iter().any(|p| !p.rels.is_empty()));
    let mut stmts: Vec<String> = Vec::new();
    stmts.push(cypher_nodes_ddl());
    if has_rels {
        stmts.push(cypher_edges_ddl());
    }

    for create in &cc {
        let mut global_node_vars: Vec<String> = Vec::new();
        let mut anon_ctr = 0usize;

        struct PatternNodeInfo {
            var_names: Vec<String>,
        }
        let mut pattern_infos: Vec<PatternNodeInfo> = Vec::new();

        for pattern in &create.patterns {
            let mut var_names = Vec::new();
            for node in &pattern.nodes {
                let var = match node.variable.as_deref() {
                    Some(v) => v.to_string(),
                    None => {
                        let n = format!("__cr{anon_ctr}");
                        anon_ctr += 1;
                        n
                    }
                };
                if !global_node_vars.contains(&var) {
                    global_node_vars.push(var.clone());
                    let labels = node_labels_sql(node);
                    let props = node_props_sql(node);
                    stmts.push(format!(
                        "INSERT INTO \"{CYPHER_NODES_TABLE}\" (\"__labels\", \"__props\") VALUES ({labels}, {props})"
                    ));
                }
                var_names.push(var);
            }
            pattern_infos.push(PatternNodeInfo { var_names });
        }

        let total = global_node_vars.len();
        let node_id_map: Vec<(String, String)> = global_node_vars
            .iter()
            .enumerate()
            .map(|(k, var)| {
                let offset = total - 1 - k;
                let id_expr = if offset == 0 {
                    format!("(SELECT max(\"__id\") FROM \"{CYPHER_NODES_TABLE}\")")
                } else {
                    format!("(SELECT max(\"__id\") - {offset} FROM \"{CYPHER_NODES_TABLE}\")")
                };
                (var.clone(), id_expr)
            })
            .collect();

        for (pi, pattern) in create.patterns.iter().enumerate() {
            let info = &pattern_infos[pi];
            for (i, rel) in pattern.rels.iter().enumerate() {
                let src_var = &info.var_names[i];
                let tgt_var = &info.var_names[i + 1];
                let src_id = node_id_map
                    .iter()
                    .find(|(v, _)| v == src_var)
                    .map_or_else(|| "0".to_string(), |(_, e)| e.clone());
                let tgt_id = node_id_map
                    .iter()
                    .find(|(v, _)| v == tgt_var)
                    .map_or_else(|| "0".to_string(), |(_, e)| e.clone());
                let rel_type = rel.rel_type.as_deref().unwrap_or("");
                if validate_rel_type(rel_type).is_err() {
                    return None;
                }
                let safe_type = escape_sq(rel_type);
                let props = rel_props_json(rel);
                let (source_id, target_id) = match rel.direction {
                    CypherDirection::Incoming => (&tgt_id, &src_id),
                    _ => (&src_id, &tgt_id),
                };
                stmts.push(format!(
                    "INSERT INTO \"{CYPHER_EDGES_TABLE}\" (\"__type\", \"__source\", \"__target\", \"__props\") \
                     VALUES ('{safe_type}', {source_id}, {target_id}, {props})"
                ));
            }
        }
    }
    Some(stmts.join("; "))
}

pub(crate) fn translate_create_return_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let CypherClause::Return(ret) = clauses.last()? else {
        return None;
    };
    let mut stmts: Vec<String> = Vec::new();
    stmts.push(cypher_nodes_ddl());
    let mut created_vars: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut has_rels = false;
    for clause in &clauses[..clauses.len() - 1] {
        if let CypherClause::Create(c) = clause {
            for pattern in &c.patterns {
                if !pattern.rels.is_empty() {
                    has_rels = true;
                }
            }
        }
    }
    if has_rels {
        stmts.push(cypher_edges_ddl());
    }
    for clause in &clauses[..clauses.len() - 1] {
        if let CypherClause::Create(c) = clause {
            for pattern in &c.patterns {
                if pattern.rels.is_empty() {
                    for node in &pattern.nodes {
                        let var = node.variable.as_deref().unwrap_or("__anon").to_string();
                        let props: Vec<(String, String)> = node
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), expr_to_sql_plain(v)))
                            .collect();
                        stmts.push(format!(
                            "INSERT INTO \"__cypher_nodes\" (\"__labels\", \"__props\") VALUES ({}, {})",
                            node_labels_sql(node), node_props_sql(node)
                        ));
                        created_vars.push((var, props));
                    }
                } else {
                    let mut node_id_exprs: Vec<(String, String)> = Vec::new();
                    for node in &pattern.nodes {
                        let var = node.variable.as_deref().unwrap_or("__anon");
                        if node_id_exprs.iter().any(|(v, _)| v == var) {
                            continue;
                        }
                        stmts.push(format!(
                            "INSERT INTO \"{CYPHER_NODES_TABLE}\" (\"__labels\", \"__props\") VALUES ({}, {})",
                            node_labels_sql(node), node_props_sql(node)
                        ));
                        let id_expr =
                            format!("(SELECT max(\"__id\") FROM \"{CYPHER_NODES_TABLE}\")");
                        node_id_exprs.push((var.to_string(), id_expr));
                        let node_props: Vec<(String, String)> = node
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), expr_to_sql_plain(v)))
                            .collect();
                        created_vars.push((var.to_string(), node_props));
                    }
                    for (i, rel) in pattern.rels.iter().enumerate() {
                        let src_var = pattern.nodes[i].variable.as_deref().unwrap_or("__anon");
                        let tgt_var = pattern.nodes[i + 1].variable.as_deref().unwrap_or("__anon");
                        let src_id = node_id_exprs
                            .iter()
                            .find(|(v, _)| v == src_var)
                            .map_or_else(|| "0".to_string(), |(_, e)| e.clone());
                        let tgt_id = node_id_exprs
                            .iter()
                            .find(|(v, _)| v == tgt_var)
                            .map_or_else(|| "0".to_string(), |(_, e)| e.clone());
                        let rel_type = rel.rel_type.as_deref().unwrap_or("");
                        if validate_rel_type(rel_type).is_err() {
                            return None;
                        }
                        let safe_type = escape_sq(rel_type);
                        let props = rel_props_json(rel);
                        let (source_id, target_id) = match rel.direction {
                            CypherDirection::Incoming => (&tgt_id, &src_id),
                            _ => (&src_id, &tgt_id),
                        };
                        stmts.push(format!(
                            "INSERT INTO \"{CYPHER_EDGES_TABLE}\" (\"__type\", \"__source\", \"__target\", \"__props\") \
                             VALUES ('{safe_type}', {source_id}, {target_id}, {props})"
                        ));
                        if let Some(ref rv) = rel.variable {
                            let rel_props: Vec<(String, String)> = rel
                                .properties
                                .iter()
                                .map(|(k, v)| (k.clone(), expr_to_sql_plain(v)))
                                .collect();
                            created_vars.push((rv.clone(), rel_props));
                        }
                    }
                }
            }
        }
    }
    let items: Vec<String> = ret
        .items
        .iter()
        .map(|item| {
            let sql_expr = create_return_expr(&item.expr, &created_vars);
            let alias = item
                .alias
                .as_deref()
                .map_or_else(|| cypher_return_alias(&item.expr), String::from);
            format!("{sql_expr} AS {}", qi(&alias))
        })
        .collect();
    let mut sel = String::from("SELECT ");
    if ret.distinct {
        sel.push_str("DISTINCT ");
    }
    sel.push_str(&items.join(", "));
    append_skip_limit(&mut sel, ret.skip.as_ref(), ret.limit.as_ref());
    stmts.push(sel);
    Some(stmts.join("; "))
}

fn create_return_expr(expr: &Expr, nodes: &[(String, Vec<(String, String)>)]) -> String {
    match expr {
        Expr::Identifier(name) if name.parts.len() == 2 => {
            let var = &name.parts[0];
            let prop = &name.parts[1];
            for (v, props) in nodes {
                if v == var {
                    for (k, val) in props {
                        if k == prop {
                            return val.clone();
                        }
                    }
                    return "NULL".to_string();
                }
            }
            expr_to_sql_plain(expr)
        }
        _ => expr_to_sql_plain(expr),
    }
}
