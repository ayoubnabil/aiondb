//! RETURN, WITH, UNWIND, UNION clause translation.

#![allow(clippy::match_same_arms, clippy::unnecessary_wraps)]

use std::collections::HashMap;

use aiondb_parser::ast::Expr;
use aiondb_parser::cypher_ast::{
    CypherClause, CypherReturnClause, CypherReturnItem, CypherUnwindClause, CypherWithClause,
};

use super::escape::qi;
use super::expr::{append_order_by, append_skip_limit, expr_to_sql_plain, expr_to_sql_with_scope};

pub(crate) fn translate_return_item(item: &CypherReturnItem) -> String {
    let e = expr_to_sql_plain(&item.expr);
    if let Some(ref alias) = item.alias {
        format!("{} AS {}", e, qi(alias))
    } else {
        e
    }
}

pub(crate) fn translate_return(ret: &CypherReturnClause) -> String {
    let mut sql = String::from("SELECT ");
    if ret.distinct {
        sql.push_str("DISTINCT ");
    }
    let items: Vec<String> = ret.items.iter().map(translate_return_item).collect();
    sql.push_str(&items.join(", "));
    append_order_by(&mut sql, &ret.order_by);
    append_skip_limit(&mut sql, ret.skip.as_ref(), ret.limit.as_ref());
    sql
}

pub(crate) fn translate_return_from_source(
    ret: &CypherReturnClause,
    source: &str,
) -> Option<String> {
    if !ret.distinct && ret.order_by.is_empty() && ret.skip.is_none() && ret.limit.is_none() {
        let all_simple = ret.items.iter().all(|item| {
            item.alias.is_none()
                && matches!(&item.expr, Expr::Identifier(name) if name.parts.len() == 1)
        });
        if all_simple {
            return Some(source.to_string());
        }
    }
    let mut sql = String::from("SELECT ");
    if ret.distinct {
        sql.push_str("DISTINCT ");
    }
    let items: Vec<String> = ret.items.iter().map(translate_return_item).collect();
    sql.push_str(&items.join(", "));
    sql.push_str(" FROM (");
    sql.push_str(source);
    sql.push_str(") AS __cypher_src");
    append_order_by(&mut sql, &ret.order_by);
    append_skip_limit(&mut sql, ret.skip.as_ref(), ret.limit.as_ref());
    Some(sql)
}

fn translate_return_from_source_with_scope(
    ret: &CypherReturnClause,
    source: &str,
    scope: &HashMap<String, String>,
) -> Option<String> {
    if scope.is_empty() {
        return translate_return_from_source(ret, source);
    }
    if !ret.distinct && ret.order_by.is_empty() && ret.skip.is_none() && ret.limit.is_none() {
        let all_simple = ret.items.iter().all(|item| {
            item.alias.is_none()
                && matches!(&item.expr, Expr::Identifier(name) if name.parts.len() == 1)
        });
        if all_simple {
            return Some(source.to_string());
        }
    }
    let mut sql = String::from("SELECT ");
    if ret.distinct {
        sql.push_str("DISTINCT ");
    }
    let items: Vec<String> = ret
        .items
        .iter()
        .map(|item| {
            let e = expr_to_sql_with_scope(&item.expr, scope);
            if let Some(ref alias) = item.alias {
                format!("{} AS {}", e, qi(alias))
            } else {
                e
            }
        })
        .collect();
    sql.push_str(&items.join(", "));
    sql.push_str(" FROM (");
    sql.push_str(source);
    sql.push_str(") AS __cypher_src");
    append_order_by(&mut sql, &ret.order_by);
    append_skip_limit(&mut sql, ret.skip.as_ref(), ret.limit.as_ref());
    Some(sql)
}

pub(crate) fn translate_unwind_with_source(
    u: &CypherUnwindClause,
    source: Option<&str>,
) -> Option<String> {
    let var = qi(&u.variable);
    let ue = expr_to_sql_plain(&u.expr);
    match source {
        None => Some(format!("SELECT unnest({ue}) AS {var}")),
        Some(prev) => {
            // For ARRAY[...] literals, the existing comma-join works.
            // For column refs or sub-expressions that quote prior
            // bindings, we need LATERAL so the inner unnest can see the
            // outer row's columns.
            if ue.starts_with("ARRAY[") {
                Some(format!(
                    "SELECT *, {var} FROM ({prev}) AS __cypher_src, unnest({ue}) AS {var}"
                ))
            } else {
                Some(format!(
                    "SELECT __cypher_src.*, {var} FROM ({prev}) AS __cypher_src \
                     CROSS JOIN LATERAL unnest({ue}) AS {var}"
                ))
            }
        }
    }
}

pub(crate) fn translate_with(w: &CypherWithClause, source: Option<&str>) -> Option<String> {
    if !w.order_by.is_empty() {
        let projected: Vec<String> = w
            .items
            .iter()
            .map(|item| {
                item.alias.clone().unwrap_or_else(|| {
                    if let Expr::Identifier(name) = &item.expr {
                        name.parts.last().cloned().unwrap_or_default()
                    } else {
                        String::new()
                    }
                })
            })
            .collect();
        for o in &w.order_by {
            if let Expr::Identifier(name) = &o.expr {
                if name.parts.len() == 1 && !projected.contains(&name.parts[0]) {
                    return None;
                }
            }
        }
    }
    let mut sql = String::from("SELECT ");
    if w.distinct {
        sql.push_str("DISTINCT ");
    }
    let items: Vec<String> = w.items.iter().map(translate_return_item).collect();
    sql.push_str(&items.join(", "));
    if let Some(src) = source {
        sql.push_str(" FROM (");
        sql.push_str(src);
        sql.push_str(") AS __cypher_src");
    }
    append_order_by(&mut sql, &w.order_by);
    append_skip_limit(&mut sql, w.skip.as_ref(), w.limit.as_ref());
    if let Some(ref wh) = w.where_clause {
        let inner = sql;
        sql = format!(
            "SELECT * FROM ({}) AS __with_src WHERE {}",
            inner,
            expr_to_sql_plain(wh)
        );
    }
    Some(sql)
}

pub(crate) fn translate_clauses(clauses: &[CypherClause]) -> Option<String> {
    if clauses.len() > 32 {
        return None;
    }
    if clauses.iter().any(|c| matches!(c, CypherClause::Merge(_))) {
        return super::mutate_translate::translate_merge_pipeline(clauses);
    }
    if clauses.iter().any(|c| matches!(c, CypherClause::Match(_))) {
        return super::match_translate::translate_match_pipeline(clauses);
    }
    if clauses.iter().any(|c| matches!(c, CypherClause::Create(_))) {
        return super::create_translate::translate_create_return_pipeline(clauses);
    }
    if let Some(call_sql) = super::call_translate::translate_call_pipeline(clauses) {
        return Some(call_sql);
    }
    let last = clauses.last()?;
    let CypherClause::Return(ret) = last else {
        return None;
    };
    let prefix = &clauses[..clauses.len() - 1];
    if prefix.is_empty() {
        return Some(translate_return(ret));
    }
    let mut unwind_parts: Vec<String> = Vec::new();
    let mut src: Option<String> = None;
    let mut scope: HashMap<String, String> = HashMap::new();
    for clause in prefix {
        match clause {
            CypherClause::Unwind(u) => {
                // After UNWIND, the variable refers to the *bound row* from
                // unnest, not the source list. Mapping to the source via
                // scope substitution would re-inline the whole list and
                // break per-row functions like `toBoolean(b)` over `b`.
                // Identity-map so identifier lookups fall through to the
                // SQL column reference.
                scope.insert(u.variable.clone(), qi(&u.variable));
                if src.is_some() {
                    src = Some(translate_unwind_with_source(u, src.as_deref())?);
                } else {
                    let var = qi(&u.variable);
                    let ue = expr_to_sql_plain(&u.expr);
                    unwind_parts.push(format!("unnest({ue}) AS {var}"));
                }
            }
            CypherClause::With(w) => {
                if !unwind_parts.is_empty() {
                    let fc = unwind_parts.join(", ");
                    src = Some(format!("SELECT * FROM {fc}"));
                    unwind_parts.clear();
                }
                src = Some(translate_with(w, src.as_deref())?);
                let prev_scope = scope.clone();
                scope.clear();
                for item in &w.items {
                    let alias = item.alias.clone().unwrap_or_else(|| {
                        if let Expr::Identifier(name) = &item.expr {
                            name.parts.last().cloned().unwrap_or_default()
                        } else {
                            String::new()
                        }
                    });
                    if !alias.is_empty() {
                        let full_expr = expr_to_sql_with_scope(&item.expr, &prev_scope);
                        scope.insert(alias, full_expr);
                    }
                }
            }
            CypherClause::Call(_) => return None,
            _ => return None,
        }
    }
    if !unwind_parts.is_empty() {
        if src.is_some() {
            for part in &unwind_parts {
                let prev = src.as_deref().unwrap_or("");
                src = Some(format!("SELECT * FROM ({prev}) AS __cypher_src, {part}"));
            }
        } else {
            let fc = unwind_parts.join(", ");
            src = Some(format!("SELECT * FROM {fc}"));
        }
    }
    if let Some(source) = src {
        translate_return_from_source_with_scope(ret, &source, &scope)
    } else {
        Some(translate_return(ret))
    }
}

pub(crate) fn extract_return_aliases(clauses: &[CypherClause]) -> Vec<String> {
    for clause in clauses.iter().rev() {
        if let CypherClause::Return(ret) = clause {
            return ret
                .items
                .iter()
                .map(|item| {
                    item.alias
                        .clone()
                        .unwrap_or_else(|| expr_to_sql_plain(&item.expr))
                })
                .collect();
        }
    }
    Vec::new()
}

pub(crate) fn validate_union_consistency(
    stmt: &aiondb_parser::cypher_ast::CypherStatement,
) -> Result<(), super::CypherTranslateError> {
    let Some(ref first_union) = stmt.union else {
        return Ok(());
    };
    let first_all = first_union.all;
    let mut current = &first_union.right;
    while let Some(ref u) = current.union {
        if u.all != first_all {
            return Err(super::CypherTranslateError::SemanticError(
                "invalid combination of UNION and UNION ALL".into(),
            ));
        }
        current = &u.right;
    }
    Ok(())
}

pub(crate) fn validate_union_columns(
    stmt: &aiondb_parser::cypher_ast::CypherStatement,
) -> Result<(), super::CypherTranslateError> {
    let left_aliases = extract_return_aliases(&stmt.clauses);
    let Some(ref union) = stmt.union else {
        return Ok(());
    };
    let mut current_right = &union.right;
    loop {
        let right_aliases = extract_return_aliases(&current_right.clauses);
        if left_aliases != right_aliases {
            return Err(super::CypherTranslateError::SemanticError(
                "all sub queries in a UNION must have the same column names".into(),
            ));
        }
        if let Some(ref next_union) = current_right.union {
            current_right = &next_union.right;
        } else {
            break;
        }
    }
    Ok(())
}
