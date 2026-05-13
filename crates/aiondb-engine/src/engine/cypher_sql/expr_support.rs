#![allow(clippy::match_same_arms, clippy::wildcard_imports)]

use super::*;

pub(crate) fn lit_to_sql(lit: &Literal) -> String {
    match lit {
        Literal::Integer(n) => n.to_string(),
        Literal::NumericLit(s) => s.clone(),
        Literal::String(s) => format!("'{}'", escape_sq(s)),
        Literal::Boolean(b) => b.to_string(),
        Literal::Null => "NULL".into(),
    }
}

pub(crate) fn is_array_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Array { .. })
}

pub(crate) fn is_string_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Literal(Literal::String(_), _))
}

pub(crate) fn is_match_property_access(expr: &Expr, nv: &[String]) -> bool {
    if let Expr::Identifier(name) = expr {
        name.parts.len() == 2 && nv.contains(&name.parts[0])
    } else {
        false
    }
}

pub(crate) fn cast_for_arith(sql: &str) -> String {
    format!("({sql})::numeric")
}

pub(crate) fn cypher_quantifier_var(expr: &Expr) -> String {
    if let Expr::Literal(Literal::String(s), _) = expr {
        qi(s)
    } else {
        "x".into()
    }
}

pub(super) fn extract_var_name(expr: &Expr) -> String {
    if let Expr::Literal(Literal::String(s), _) = expr {
        s.clone()
    } else {
        String::new()
    }
}

pub(crate) fn cypher_return_alias(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(name) => name.parts.join("."),
        Expr::FunctionCall { name, args, .. } => {
            let a: Vec<String> = args.iter().map(cypher_return_alias).collect();
            format!("{}({})", name.parts.join("."), a.join(", "))
        }
        _ => expr_to_sql_plain(expr),
    }
}

pub(crate) fn cypher_expr_to_json_value(expr: &Expr) -> String {
    match expr {
        Expr::Literal(lit, _) => match lit {
            Literal::Integer(n) => n.to_string(),
            Literal::NumericLit(s) => s.clone(),
            Literal::String(s) => {
                let e = escape_json_key(s);
                format!("\"{e}\"")
            }
            Literal::Boolean(b) => b.to_string(),
            Literal::Null => "null".into(),
        },
        Expr::Array { elements, .. } => {
            let e: Vec<String> = elements.iter().map(cypher_expr_to_json_value).collect();
            format!("[{}]", e.join(", "))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
            ..
        } => {
            format!("-{}", cypher_expr_to_json_value(expr))
        }
        _ => "null".into(),
    }
}

pub(crate) fn temporal_property_to_extract(raw_field: &str, base: &str) -> Option<String> {
    let field = raw_field.to_ascii_lowercase();
    let ef = match field.as_str() {
        "year" => "YEAR",
        "month" => "MONTH",
        "day" => "DAY",
        "hour" => "HOUR",
        "minute" => "MINUTE",
        "second" => "SECOND",
        "quarter" => "QUARTER",
        "week" => "WEEK",
        "weekyear" => return Some(format!("EXTRACT(ISOYEAR FROM {base})::bigint")),
        "weekday" | "dayofweek" => return Some(format!("EXTRACT(ISODOW FROM {base})::bigint")),
        "ordinalday" | "dayofyear" => return Some(format!("EXTRACT(DOY FROM {base})::bigint")),
        "dayofquarter" => {
            return Some(format!(
                "(EXTRACT(DOY FROM {base})::bigint - EXTRACT(DOY FROM date_trunc('quarter', {base}::timestamp))::bigint + 1)"
            ))
        }
        "millisecond" => {
            return Some(format!("(EXTRACT(MILLISECOND FROM {base})::bigint % 1000)"))
        }
        "microsecond" => {
            return Some(format!("(EXTRACT(MICROSECOND FROM {base})::bigint % 1000000)"))
        }
        "nanosecond" => {
            return Some(format!(
                "(EXTRACT(MICROSECOND FROM {base})::bigint % 1000000 * 1000)"
            ))
        }
        "offset" | "offsetminutes" => {
            return Some(format!("(EXTRACT(TIMEZONE FROM {base})::bigint / 60)"))
        }
        "offsetseconds" => return Some(format!("EXTRACT(TIMEZONE FROM {base})::bigint")),
        "months" => {
            return Some(format!(
                "(EXTRACT(YEAR FROM {base})::bigint * 12 + EXTRACT(MONTH FROM {base})::bigint)"
            ))
        }
        "days" => return Some(format!("EXTRACT(DAY FROM {base})::bigint")),
        "seconds" => {
            return Some(format!(
                "(EXTRACT(HOUR FROM {base})::bigint * 3600 + EXTRACT(MINUTE FROM {base})::bigint * 60 + EXTRACT(SECOND FROM {base})::bigint)"
            ))
        }
        "secondsofminute" => return Some(format!("EXTRACT(SECOND FROM {base})::bigint")),
        "minutesofhour" => return Some(format!("EXTRACT(MINUTE FROM {base})::bigint")),
        "hoursofday" => return Some(format!("EXTRACT(HOUR FROM {base})::bigint")),
        "monthsofyear" => return Some(format!("EXTRACT(MONTH FROM {base})::bigint")),
        "years" => return Some(format!("EXTRACT(YEAR FROM {base})::bigint")),
        "nanosecondsofsecond" | "nanosofsecond" => {
            return Some(format!(
                "((EXTRACT(MICROSECOND FROM {base})::bigint % 1000000) * 1000)"
            ))
        }
        "microsecondsofsecond" | "microsofsecond" => {
            return Some(format!("(EXTRACT(MICROSECOND FROM {base})::bigint % 1000000)"))
        }
        "millisecondsofsecond" | "millisofsecond" => {
            return Some(format!("(EXTRACT(MILLISECOND FROM {base})::bigint % 1000)"))
        }
        _ => return None,
    };
    Some(format!("EXTRACT({ef} FROM {base})::bigint"))
}

/// Format the ORDER BY direction + NULLS FIRST/LAST suffix for one sort key.
/// Caller already rendered the expression string; this helper just appends
/// the optional `" DESC"` and `" NULLS FIRST"|" NULLS LAST"` parts.
pub(crate) fn order_by_suffix(descending: bool, nulls_first: Option<bool>) -> &'static str {
    match (descending, nulls_first) {
        (true, Some(true)) => " DESC NULLS FIRST",
        (true, Some(false)) => " DESC NULLS LAST",
        (true, None) => " DESC",
        (false, Some(true)) => " NULLS FIRST",
        (false, Some(false)) => " NULLS LAST",
        (false, None) => "",
    }
}

pub(crate) fn append_order_by(sql: &mut String, order_by: &[aiondb_parser::ast::OrderByItem]) {
    if order_by.is_empty() {
        return;
    }
    sql.push_str(" ORDER BY ");
    let items: Vec<String> = order_by
        .iter()
        .map(|o| {
            let e = expr_to_sql_plain(&o.expr);
            format!("{e}{}", order_by_suffix(o.descending, o.nulls_first))
        })
        .collect();
    sql.push_str(&items.join(", "));
}

pub(crate) fn append_skip_limit(sql: &mut String, skip: Option<&Expr>, limit: Option<&Expr>) {
    if let Some(s) = skip {
        sql.push_str(" OFFSET ");
        sql.push_str(&expr_to_sql_plain(s));
    }
    if let Some(l) = limit {
        sql.push_str(" LIMIT ");
        sql.push_str(&expr_to_sql_plain(l));
    }
}

pub(super) fn try_simple_any_all(
    var_arg: &Expr,
    list_arg: &Expr,
    pred_arg: &Expr,
    kind: &str,
) -> Option<String> {
    let var_name = if let Expr::Literal(Literal::String(s), _) = var_arg {
        s.as_str()
    } else {
        return None;
    };
    let list_sql = expr_to_sql_plain(list_arg);
    let list_is_col_ref = matches!(list_arg, Expr::Identifier(name) if name.parts.len() == 1);
    if !list_is_col_ref {
        return None;
    }
    if let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
        ..
    } = pred_arg
    {
        let val_expr = if is_ident_matching(left, var_name) {
            right.as_ref()
        } else if is_ident_matching(right, var_name) {
            left.as_ref()
        } else {
            return None;
        };
        let val_sql = expr_to_sql_plain(val_expr);
        match kind {
            "any" => Some(format!("({val_sql} = ANY({list_sql}))")),
            "all" => Some(format!("({val_sql} = ALL({list_sql}))")),
            "none" => Some(format!("(NOT ({val_sql} = ANY({list_sql})))")),
            "single" => Some(format!(
                "(array_position({list_sql}, {val_sql}) IS NOT NULL AND array_position({list_sql}, {val_sql}, array_position({list_sql}, {val_sql}) + 1) IS NULL)"
            )),
            _ => None,
        }
    } else if let Expr::BinaryOp {
        left,
        op: BinaryOperator::Ne,
        right,
        ..
    } = pred_arg
    {
        let val_expr = if is_ident_matching(left, var_name) {
            right.as_ref()
        } else if is_ident_matching(right, var_name) {
            left.as_ref()
        } else {
            return None;
        };
        let val_sql = expr_to_sql_plain(val_expr);
        match kind {
            "any" => Some(format!("(NOT ({val_sql} = ALL({list_sql})))")),
            "all" => Some(format!("(NOT ({val_sql} = ANY({list_sql})))")),
            "none" => Some(format!("({val_sql} = ALL({list_sql}))")),
            _ => None,
        }
    } else {
        None
    }
}

fn is_ident_matching(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Identifier(n) if n.parts.len() == 1 && n.parts[0] == name)
}
