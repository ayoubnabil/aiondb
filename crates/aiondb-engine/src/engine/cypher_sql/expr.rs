//! Unified expression-to-SQL translation for Cypher AST.
//!
//! Provides a single `expr_to_sql` dispatcher with an optional `CypherSqlContext`
//! that carries node/relationship variable lists and scope mappings, replacing
//! the former four separate expression functions.

#![allow(clippy::match_same_arms, clippy::too_many_lines)]

use std::collections::HashMap;

use aiondb_core::{DataType, VectorValue};
use aiondb_parser::ast::{BinaryOperator, Expr, Literal, UnaryOperator};

use super::escape::{escape_json_key, escape_sq, qi};
use super::UNSUPPORTED_CYPHER_PATTERN_COMPREHENSION_SENTINEL;
#[path = "expr_function_call.rs"]
mod expr_function_call;
#[path = "expr_support.rs"]
mod expr_support;
use self::expr_function_call::translate_function_call;
pub(crate) use self::expr_support::{
    append_order_by, append_skip_limit, cast_for_arith, cypher_expr_to_json_value,
    cypher_quantifier_var, cypher_return_alias, is_array_expr, is_match_property_access,
    is_string_expr, lit_to_sql, order_by_suffix, temporal_property_to_extract,
};
use self::expr_support::{extract_var_name, try_simple_any_all};

/// Context for expression translation.
pub(crate) struct CypherSqlContext {
    pub node_vars: Vec<String>,
    pub rel_vars: Vec<String>,
    pub scope: HashMap<String, String>,
}

impl CypherSqlContext {
    pub(crate) fn empty() -> Self {
        Self {
            node_vars: Vec::new(),
            rel_vars: Vec::new(),
            scope: HashMap::new(),
        }
    }

    pub(crate) fn with_scope(scope: HashMap<String, String>) -> Self {
        Self {
            node_vars: Vec::new(),
            rel_vars: Vec::new(),
            scope,
        }
    }

    pub(crate) fn from_match(node_vars: Vec<String>, rel_vars: Vec<String>) -> Self {
        Self {
            node_vars,
            rel_vars,
            scope: HashMap::new(),
        }
    }

    fn has_match_context(&self) -> bool {
        !self.node_vars.is_empty() || !self.rel_vars.is_empty()
    }
}

// ── Main entry point ───────────────────────────────────────────────

/// Hard-cap depth for Cypher → SQL expression translation. The
/// translator recurses on every nested operator / function-call /
/// list-comprehension; without a cap a 50k-deep `a+a+a+…` would blow
/// the host stack before reaching the planner. 256 covers any
/// hand-written Cypher expression by an order of magnitude.
const CYPHER_EXPR_TO_SQL_MAX_DEPTH: u32 = 256;

thread_local! {
    static CYPHER_EXPR_TO_SQL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct CypherExprDepthGuard;

impl CypherExprDepthGuard {
    fn enter() -> Result<Self, ()> {
        let too_deep = CYPHER_EXPR_TO_SQL_DEPTH.with(|c| {
            let next = c.get().saturating_add(1);
            if next > CYPHER_EXPR_TO_SQL_MAX_DEPTH {
                true
            } else {
                c.set(next);
                false
            }
        });
        if too_deep {
            Err(())
        } else {
            Ok(Self)
        }
    }
}

impl Drop for CypherExprDepthGuard {
    fn drop(&mut self) {
        CYPHER_EXPR_TO_SQL_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

pub(crate) fn expr_to_sql(expr: &Expr, ctx: &CypherSqlContext) -> String {
    let Ok(_guard) = CypherExprDepthGuard::enter() else {
        // Sentinel string: the calling SQL planner will reject it as
        // an unparseable identifier rather than blowing the stack.
        return "/* aiondb: cypher expression exceeded translation depth */".to_string();
    };
    if !ctx.scope.is_empty() {
        if let Some(s) = try_scope_substitution(expr, ctx) {
            return s;
        }
    }
    if ctx.has_match_context() {
        if let Some(s) = try_match_expr(expr, ctx) {
            return s;
        }
    }
    expr_to_sql_generic(expr, ctx)
}

/// The "plain" `expr_to_sql`: no match context, no scope.
pub(crate) fn expr_to_sql_plain(expr: &Expr) -> String {
    expr_to_sql(expr, &CypherSqlContext::empty())
}

/// `expr_to_sql` with a scope (used for UNWIND/WITH quantifier inlining).
pub(crate) fn expr_to_sql_with_scope(expr: &Expr, scope: &HashMap<String, String>) -> String {
    if scope.is_empty() {
        return expr_to_sql_plain(expr);
    }
    let ctx = CypherSqlContext::with_scope(scope.clone());
    expr_to_sql(expr, &ctx)
}

/// Match-aware expression for WHERE clauses.
pub(crate) fn match_expr_to_sql(expr: &Expr, nv: &[String], rv: &[String]) -> String {
    let Ok(_guard) = CypherExprDepthGuard::enter() else {
        return "/* aiondb: cypher expression exceeded translation depth */".to_string();
    };
    let ctx = CypherSqlContext::from_match(nv.to_vec(), rv.to_vec());
    match_expr_to_sql_inner(expr, &ctx)
}

/// Match-aware expression for RETURN items (includes node/rel formatting).
pub(crate) fn match_return_item_to_sql(expr: &Expr, nv: &[String], rv: &[String]) -> String {
    let Ok(_guard) = CypherExprDepthGuard::enter() else {
        return "/* aiondb: cypher expression exceeded translation depth */".to_string();
    };
    let ctx = CypherSqlContext::from_match(nv.to_vec(), rv.to_vec());
    match_return_item_inner(expr, &ctx)
}

// ── Scope substitution ─────────────────────────────────────────────

fn try_scope_substitution(expr: &Expr, ctx: &CypherSqlContext) -> Option<String> {
    match expr {
        Expr::Identifier(name) if name.parts.len() == 1 => ctx.scope.get(&name.parts[0]).cloned(),
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            let fn_name_lower = name.parts.join(".").to_lowercase();
            match fn_name_lower.as_str() {
                "__cypher_any" | "__cypher_all" | "__cypher_none" | "__cypher_single"
                    if args.len() == 3 =>
                {
                    Some(translate_scope_quantifier(&fn_name_lower, args, ctx))
                }
                "__cypher_list_comprehension" if args.len() == 4 => {
                    let var = cypher_quantifier_var(&args[0]);
                    let var_name = extract_var_name(&args[0]);
                    let mut inner_scope = ctx.scope.clone();
                    inner_scope.remove(&var_name);
                    let list = expr_to_sql_with_scope(&args[1], &ctx.scope);
                    let inner_ctx = CypherSqlContext::with_scope(inner_scope);
                    let pred = expr_to_sql(&args[2], &inner_ctx);
                    let map_e = expr_to_sql(&args[3], &inner_ctx);
                    Some(format!("COALESCE((SELECT array_agg({map_e}) FROM unnest({list}) AS {var} WHERE {pred}), ARRAY[]::bigint[])"))
                }
                // `dur.days` / `node.prop` style property access. The base
                // must be substituted via scope (e.g. `dur` → SQL for the
                // bound duration), and temporal properties get rewritten
                // to EXTRACT(...) form just like the non-scope path.
                // Cypher `list[lo..hi]` slicing: translate the same way
                // as the non-scope path so the runtime never sees the
                // raw composite call (which keeps PG 1-indexed inclusive
                // semantics and yields off-by-one results).
                "__aiondb_array_slice" if args.len() == 5 => {
                    let base = expr_to_sql(&args[0], ctx);
                    let lower = expr_to_sql(&args[1], ctx);
                    let upper = expr_to_sql(&args[2], ctx);
                    let lo_omit = matches!(&args[3], Expr::Literal(Literal::Boolean(true), _));
                    let up_omit = matches!(&args[4], Expr::Literal(Literal::Boolean(true), _));
                    if lo_omit && up_omit {
                        Some(base)
                    } else if (!lo_omit && matches!(&args[1], Expr::Literal(Literal::Null, _)))
                        || (!up_omit && matches!(&args[2], Expr::Literal(Literal::Null, _)))
                    {
                        Some("NULL".to_string())
                    } else {
                        let lo_sql = if lo_omit {
                            "1".to_string()
                        } else {
                            format!("(CASE WHEN ({lower}) >= 0 THEN ({lower}) + 1 ELSE GREATEST(coalesce(array_length({base}, 1), 0) + ({lower}) + 1, 1) END)")
                        };
                        let hi_sql = if up_omit {
                            format!("coalesce(array_length({base}, 1), 0)")
                        } else {
                            format!("(CASE WHEN ({upper}) >= 0 THEN ({upper}) ELSE coalesce(array_length({base}, 1), 0) + ({upper}) END)")
                        };
                        Some(format!("({base})[{lo_sql}:{hi_sql}]"))
                    }
                }
                "__aiondb_composite_field" if args.len() == 2 => {
                    let base = expr_to_sql(&args[0], ctx);
                    if let Expr::Literal(Literal::String(field), _) = &args[1] {
                        if let Some(es) = temporal_property_to_extract(field, &base) {
                            Some(es)
                        } else {
                            Some(format!("(({base})->>'{}')", escape_sq(field)))
                        }
                    } else {
                        let field_s = expr_to_sql(&args[1], ctx);
                        Some(format!("(({base})->>{field_s})"))
                    }
                }
                _ => {
                    let a: Vec<String> = args.iter().map(|x| expr_to_sql(x, ctx)).collect();
                    let d = if *distinct { "DISTINCT " } else { "" };
                    // Mangle Cypher temporal constructor names so they
                    // resolve to `cypher_date`/`cypher_time`/... (and not
                    // the PG `date(text)` cast in pg_internal_info).
                    let dotted = name.parts.join(".");
                    let dispatch = match dotted.to_ascii_lowercase().as_str() {
                        "date" | "time" | "localtime" | "datetime" | "localdatetime"
                        | "duration" => format!("\"cypher_{}\"", dotted.to_ascii_lowercase()),
                        _ => dotted,
                    };
                    let mut s = format!("{}({d}{})", dispatch, a.join(", "));
                    if let Some(f) = filter {
                        // Stream " FILTER (WHERE …)" into `s` instead
                        // of allocating a transient `format!` String.
                        use std::fmt::Write;
                        let _ = write!(s, " FILTER (WHERE {})", expr_to_sql(f, ctx));
                    }
                    Some(s)
                }
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let l = expr_to_sql(left, ctx);
            let r = expr_to_sql(right, ctx);
            if matches!(op, BinaryOperator::Add) {
                return Some(
                    if is_array_expr(left)
                        || is_array_expr(right)
                        || is_string_expr(left)
                        || is_string_expr(right)
                    {
                        format!("({l} || {r})")
                    } else {
                        format!("({l} + {r})")
                    },
                );
            }
            if matches!(op, BinaryOperator::Div) {
                return Some(format!(
                    "(CASE WHEN ({r})::float8 = 0 THEN \
                     CASE WHEN ({l})::float8 = 0 THEN ('NaN'::float8) ELSE NULL END \
                     ELSE ({l})::float8 / ({r})::float8 END)"
                ));
            }
            if matches!(op, BinaryOperator::Mod) {
                return Some(format!(
                    "(CASE WHEN ({r}) = 0 THEN NULL ELSE ({l}) % ({r}) END)"
                ));
            }
            let o = match op {
                BinaryOperator::Add => "+",
                BinaryOperator::Sub => "-",
                BinaryOperator::Mul => "*",
                BinaryOperator::Div => "/",
                BinaryOperator::Mod => "%",
                BinaryOperator::Eq => "=",
                BinaryOperator::Ne => "<>",
                BinaryOperator::Lt => "<",
                BinaryOperator::Le => "<=",
                BinaryOperator::Gt => ">",
                BinaryOperator::Ge => ">=",
                BinaryOperator::And => "AND",
                BinaryOperator::Or => "OR",
                BinaryOperator::Concat => "||",
                _ => return None,
            };
            Some(format!("({l} {o} {r})"))
        }
        Expr::UnaryOp {
            op, expr: inner, ..
        } => {
            let s = expr_to_sql(inner, ctx);
            Some(match op {
                UnaryOperator::Not => format!("(NOT ({s}))"),
                UnaryOperator::Minus => format!("-({s})"),
                _ => return None,
            })
        }
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            let s = expr_to_sql(inner, ctx);
            Some(if *negated {
                format!("(({s}) IS NOT NULL)")
            } else {
                format!("(({s}) IS NULL)")
            })
        }
        Expr::Array { elements, .. } => {
            let e: Vec<String> = elements.iter().map(|x| expr_to_sql(x, ctx)).collect();
            Some(if e.is_empty() {
                "ARRAY[]::bigint[]".into()
            } else {
                format!("ARRAY[{}]", e.join(", "))
            })
        }
        Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => Some(format!(
            "CAST({} AS {})",
            expr_to_sql(inner, ctx),
            data_type
        )),
        _ => None,
    }
}

fn translate_scope_quantifier(fn_name: &str, args: &[Expr], ctx: &CypherSqlContext) -> String {
    let var = cypher_quantifier_var(&args[0]);
    let var_name = extract_var_name(&args[0]);
    let mut inner_scope = ctx.scope.clone();
    inner_scope.remove(&var_name);
    let list = expr_to_sql_with_scope(&args[1], &ctx.scope);
    let inner_ctx = CypherSqlContext::with_scope(inner_scope);
    let pred = expr_to_sql(&args[2], &inner_ctx);
    match fn_name {
        "__cypher_any" => format!("(SELECT CASE WHEN count(*) = 0 THEN false ELSE bool_or({pred}) END FROM unnest({list}) AS {var})"),
        "__cypher_all" => format!("(SELECT CASE WHEN count(*) = 0 THEN true ELSE bool_and({pred}) END FROM unnest({list}) AS {var})"),
        "__cypher_none" => format!("(SELECT CASE WHEN count(*) = 0 THEN true ELSE NOT bool_or({pred}) END FROM unnest({list}) AS {var})"),
        "__cypher_single" => format!("((SELECT count(*) FROM unnest({list}) AS {var} WHERE {pred}) = 1)"),
        _ => "NULL".into(),
    }
}

fn is_vector_distance_function(name: &str) -> bool {
    matches!(
        name,
        "l2_distance"
            | "cosine_distance"
            | "inner_product"
            | "negative_inner_product"
            | "manhattan_distance"
    )
}

fn infer_vector_dims(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Literal(Literal::String(text), _) => VectorValue::parse(text).map(|v| v.dims),
        Expr::Cast { data_type, .. } => match data_type {
            DataType::Vector { dims, .. } => Some(*dims),
            _ => None,
        },
        _ => None,
    }
}

fn cast_match_vector_arg(expr: &Expr, ctx: &CypherSqlContext, dims: u32) -> String {
    let sql = match_expr_to_sql_inner(expr, ctx);
    match expr {
        Expr::Cast {
            data_type: DataType::Vector { .. },
            ..
        } => sql,
        Expr::Literal(Literal::String(_), _) => format!("CAST({sql} AS VECTOR({dims}))"),
        Expr::Identifier(name)
            if name.parts.len() == 2 && ctx.node_vars.contains(&name.parts[0]) =>
        {
            format!("CAST({sql} AS VECTOR({dims}))")
        }
        _ => sql,
    }
}

// ── Match-aware WHERE expression ───────────────────────────────────

fn match_expr_to_sql_inner(expr: &Expr, ctx: &CypherSqlContext) -> String {
    let nv = &ctx.node_vars;
    let rv = &ctx.rel_vars;
    match expr {
        Expr::Identifier(name) if name.parts.len() == 2 && nv.contains(&name.parts[0]) => {
            let v = qi(&name.parts[0]);
            let p = escape_sq(&name.parts[1]);
            format!("{v}.\"__props\"->>'{p}'")
        }
        Expr::FunctionCall { name, args, .. } => {
            let fn_name = name.parts.join(".").to_lowercase();
            match fn_name.as_str() {
                "type" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && rv.contains(&id.parts[0]) {
                            return format!("{}.\"__type\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "__cypher_starts_with" if args.len() == 2 => {
                    let l = match_expr_to_sql_inner(&args[0], ctx);
                    let r = match_expr_to_sql_inner(&args[1], ctx);
                    format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE ({r}) || '%') END)")
                }
                "__cypher_ends_with" if args.len() == 2 => {
                    let l = match_expr_to_sql_inner(&args[0], ctx);
                    let r = match_expr_to_sql_inner(&args[1], ctx);
                    format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE '%' || ({r})) END)")
                }
                "__cypher_contains" if args.len() == 2 => {
                    let l = match_expr_to_sql_inner(&args[0], ctx);
                    let r = match_expr_to_sql_inner(&args[1], ctx);
                    format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE '%' || ({r}) || '%') END)")
                }
                "__cypher_in" if args.len() == 2 => {
                    let l = match_expr_to_sql_inner(&args[0], ctx);
                    let r = match_expr_to_sql_inner(&args[1], ctx);
                    format!("({l} = ANY({r}))")
                }
                "__cypher_has_label" if args.len() == 2 => {
                    if let (
                        Expr::Identifier(ref id),
                        Expr::Literal(Literal::String(ref label), _),
                    ) = (&args[0], &args[1])
                    {
                        if id.parts.len() == 1
                            && (nv.contains(&id.parts[0]) || rv.contains(&id.parts[0]))
                        {
                            let v = qi(&id.parts[0]);
                            let safe = escape_sq(label);
                            return format!("('{safe}' = ANY({v}.\"__labels\"))");
                        }
                    }
                    "true".to_string()
                }
                _ if is_vector_distance_function(fn_name.as_str()) && args.len() == 2 => {
                    let dims = infer_vector_dims(&args[0]).or_else(|| infer_vector_dims(&args[1]));
                    if let Some(dims) = dims {
                        let l = cast_match_vector_arg(&args[0], ctx, dims);
                        let r = cast_match_vector_arg(&args[1], ctx, dims);
                        format!("{}({l}, {r})", name.parts.join("."))
                    } else {
                        let a: Vec<String> = args
                            .iter()
                            .map(|x| match_expr_to_sql_inner(x, ctx))
                            .collect();
                        format!("{}({})", name.parts.join("."), a.join(", "))
                    }
                }
                _ => {
                    let a: Vec<String> = args
                        .iter()
                        .map(|x| match_expr_to_sql_inner(x, ctx))
                        .collect();
                    format!("{}({})", name.parts.join("."), a.join(", "))
                }
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let is_arith = matches!(
                op,
                BinaryOperator::Add
                    | BinaryOperator::Sub
                    | BinaryOperator::Mul
                    | BinaryOperator::Div
                    | BinaryOperator::Mod
            );
            let l = match_expr_to_sql_inner(left, ctx);
            let r = match_expr_to_sql_inner(right, ctx);
            let l = if is_arith && is_match_property_access(left, nv) {
                cast_for_arith(&l)
            } else {
                l
            };
            let r = if is_arith && is_match_property_access(right, nv) {
                cast_for_arith(&r)
            } else {
                r
            };
            let o = match op {
                BinaryOperator::Eq => "=",
                BinaryOperator::Ne => "<>",
                BinaryOperator::Lt => "<",
                BinaryOperator::Le => "<=",
                BinaryOperator::Gt => ">",
                BinaryOperator::Ge => ">=",
                BinaryOperator::And => "AND",
                BinaryOperator::Or => "OR",
                BinaryOperator::Add => "+",
                BinaryOperator::Sub => "-",
                BinaryOperator::Mul => "*",
                BinaryOperator::Div => "/",
                BinaryOperator::Mod => "%",
                _ => return expr_to_sql_plain(expr),
            };
            format!("({l} {o} {r})")
        }
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            let s = match_expr_to_sql_inner(inner, ctx);
            if *negated {
                format!("(({s}) IS NOT NULL)")
            } else {
                format!("(({s}) IS NULL)")
            }
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: inner,
            ..
        } => {
            let s = match_expr_to_sql_inner(inner, ctx);
            format!("(NOT ({s}))")
        }
        _ => expr_to_sql_plain(expr),
    }
}

// ── Match-aware RETURN item expression ─────────────────────────────

fn match_return_item_inner(expr: &Expr, ctx: &CypherSqlContext) -> String {
    let nv = &ctx.node_vars;
    let rv = &ctx.rel_vars;
    match expr {
        Expr::Identifier(name) => {
            if name.parts.len() == 1 && rv.contains(&name.parts[0]) {
                let v = qi(&name.parts[0]);
                format!(
                    "CASE WHEN {v}.\"__props\" = '{{}}'::JSONB OR {v}.\"__props\" IS NULL \
                     THEN '[:' || {v}.\"__type\" || ']' \
                     ELSE '[:' || {v}.\"__type\" || ' ' || CAST({v}.\"__props\" AS TEXT) || ']' END"
                )
            } else if name.parts.len() == 1 && nv.contains(&name.parts[0]) {
                let v = qi(&name.parts[0]);
                format!(
                    "'(' || CASE WHEN {v}.\"__labels\" IS NOT NULL AND array_length({v}.\"__labels\", 1) > 0 THEN ':' || array_to_string({v}.\"__labels\", ':') ELSE '' END || CASE WHEN {v}.\"__props\" IS NOT NULL AND {v}.\"__props\" <> '{{}}'::JSONB THEN ' ' || CAST({v}.\"__props\" AS TEXT) ELSE '' END || ')'"
                )
            } else if name.parts.len() == 2 && nv.contains(&name.parts[0]) {
                let v = qi(&name.parts[0]);
                let p = escape_sq(&name.parts[1]);
                format!("{v}.\"__props\"->>'{p}'")
            } else {
                expr_to_sql_plain(expr)
            }
        }
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            let fn_name = name.parts.join(".").to_lowercase();
            match fn_name.as_str() {
                "count" if args.is_empty() => "count(*)".into(),
                "count" => {
                    let inner = match_return_item_inner(&args[0], ctx);
                    let d = if *distinct { "DISTINCT " } else { "" };
                    format!("count({d}{inner})")
                }
                "labels" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1
                            && nv.contains(&id.parts[0])
                            && !rv.contains(&id.parts[0])
                        {
                            return format!("{}.\"__labels\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "type" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && rv.contains(&id.parts[0]) {
                            return format!("{}.\"__type\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "id" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && nv.contains(&id.parts[0]) {
                            return format!("{}.\"__id\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "startnode" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && rv.contains(&id.parts[0]) {
                            return format!("{}.\"__source\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "endnode" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && rv.contains(&id.parts[0]) {
                            return format!("{}.\"__target\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "properties" if args.len() == 1 => {
                    if let Expr::Identifier(ref id) = args[0] {
                        if id.parts.len() == 1 && nv.contains(&id.parts[0]) {
                            return format!("{}.\"__props\"", qi(&id.parts[0]));
                        }
                    }
                    expr_to_sql_plain(expr)
                }
                "collect" if args.len() == 1 => {
                    let inner = match_return_item_inner(&args[0], ctx);
                    let d = if *distinct { "DISTINCT " } else { "" };
                    format!("coalesce(array_agg({d}{inner}) FILTER (WHERE ({inner}) IS NOT NULL), ARRAY[]::bigint[])")
                }
                "__cypher_has_label" if args.len() == 2 => {
                    if let (
                        Expr::Identifier(ref id),
                        Expr::Literal(Literal::String(ref label), _),
                    ) = (&args[0], &args[1])
                    {
                        if id.parts.len() == 1
                            && (nv.contains(&id.parts[0]) || rv.contains(&id.parts[0]))
                        {
                            let v = qi(&id.parts[0]);
                            let safe = escape_sq(label);
                            return format!("('{safe}' = ANY({v}.\"__labels\"))");
                        }
                    }
                    "true".to_string()
                }
                _ => {
                    let a: Vec<String> = args
                        .iter()
                        .map(|x| match_return_item_inner(x, ctx))
                        .collect();
                    let d = if *distinct { "DISTINCT " } else { "" };
                    let mut s = format!("{}({d}{})", name.parts.join("."), a.join(", "));
                    if let Some(f) = filter {
                        // Stream the FILTER clause into `s` instead of
                        // allocating a transient format!() String.
                        use std::fmt::Write;
                        let _ = write!(s, " FILTER (WHERE {})", match_return_item_inner(f, ctx));
                    }
                    s
                }
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let is_arith = matches!(
                op,
                BinaryOperator::Add
                    | BinaryOperator::Sub
                    | BinaryOperator::Mul
                    | BinaryOperator::Div
                    | BinaryOperator::Mod
            );
            let l = match_return_item_inner(left, ctx);
            let r = match_return_item_inner(right, ctx);
            let l = if is_arith && is_match_property_access(left, nv) {
                cast_for_arith(&l)
            } else {
                l
            };
            let r = if is_arith && is_match_property_access(right, nv) {
                cast_for_arith(&r)
            } else {
                r
            };
            let o = match op {
                BinaryOperator::Add => "+",
                BinaryOperator::Sub => "-",
                BinaryOperator::Mul => "*",
                BinaryOperator::Div => "/",
                BinaryOperator::Mod => "%",
                BinaryOperator::Eq => "=",
                BinaryOperator::Ne => "<>",
                BinaryOperator::Lt => "<",
                BinaryOperator::Le => "<=",
                BinaryOperator::Gt => ">",
                BinaryOperator::Ge => ">=",
                BinaryOperator::And => "AND",
                BinaryOperator::Or => "OR",
                _ => return expr_to_sql_plain(expr),
            };
            format!("({l} {o} {r})")
        }
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            let s = match_return_item_inner(inner, ctx);
            if *negated {
                format!("(({s}) IS NOT NULL)")
            } else {
                format!("(({s}) IS NULL)")
            }
        }
        Expr::UnaryOp {
            op, expr: inner, ..
        } => {
            let s = match_return_item_inner(inner, ctx);
            match op {
                UnaryOperator::Not => format!("(NOT ({s}))"),
                UnaryOperator::Minus => {
                    if is_match_property_access(inner, nv) {
                        format!("-({})", cast_for_arith(&s))
                    } else {
                        format!("-({s})")
                    }
                }
                _ => expr_to_sql_plain(expr),
            }
        }
        _ => expr_to_sql_plain(expr),
    }
}

// ── Match-specific identifier rewriting ────────────────────────────

fn try_match_expr(expr: &Expr, ctx: &CypherSqlContext) -> Option<String> {
    if let Expr::Identifier(name) = expr {
        let nv = &ctx.node_vars;
        if name.parts.len() == 2 && nv.contains(&name.parts[0]) {
            let v = qi(&name.parts[0]);
            let p = escape_sq(&name.parts[1]);
            return Some(format!("{v}.\"__props\"->>'{p}'"));
        }
    }
    None
}

// ── Generic expr_to_sql (no MATCH context) ─────────────────────────

fn expr_to_sql_generic(expr: &Expr, _ctx: &CypherSqlContext) -> String {
    match expr {
        Expr::Literal(lit, _) => lit_to_sql(lit),
        Expr::Identifier(name) if name.parts.len() == 2 => {
            let var = qi(&name.parts[0]);
            let prop = &name.parts[1];
            if let Some(sql) = temporal_property_to_extract(prop, &var) {
                sql
            } else {
                let pe = escape_sq(prop);
                format!("({var}->>'{pe}')")
            }
        }
        Expr::Identifier(name) => name
            .parts
            .iter()
            .map(|p| qi(p))
            .collect::<Vec<_>>()
            .join("."),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::Default { .. } => "DEFAULT".into(),
        Expr::UnaryOp { op, expr, .. } => {
            let inner = expr_to_sql_plain(expr);
            match op {
                UnaryOperator::Not => format!("(NOT ({inner}))"),
                UnaryOperator::Minus => format!("-({inner})"),
                UnaryOperator::BitwiseNot => format!("~({inner})"),
                UnaryOperator::Abs => format!("@({inner})"),
                UnaryOperator::SquareRoot => format!("|/({inner})"),
                UnaryOperator::CubeRoot => format!("||/({inner})"),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            if matches!(op, BinaryOperator::Add) {
                let l = expr_to_sql_plain(left);
                let r = expr_to_sql_plain(right);
                return if is_array_expr(left)
                    || is_array_expr(right)
                    || is_string_expr(left)
                    || is_string_expr(right)
                {
                    format!("({l} || {r})")
                } else {
                    format!("({l} + {r})")
                };
            }
            let l = expr_to_sql_plain(left);
            let r = expr_to_sql_plain(right);
            if matches!(op, BinaryOperator::Div) {
                return format!(
                    "(CASE WHEN ({r})::float8 = 0 THEN \
                     CASE WHEN ({l})::float8 = 0 THEN ('NaN'::float8) ELSE NULL END \
                     ELSE ({l})::float8 / ({r})::float8 END)"
                );
            }
            if matches!(op, BinaryOperator::Mod) {
                return format!("(CASE WHEN ({r}) = 0 THEN NULL ELSE ({l}) % ({r}) END)");
            }
            if matches!(op, BinaryOperator::Eq) {
                return format!("(CASE WHEN ({l})::text = 'NaN' OR ({r})::text = 'NaN' THEN false ELSE ({l}) = ({r}) END)");
            }
            if matches!(op, BinaryOperator::Ne) {
                return format!("(CASE WHEN ({l})::text = 'NaN' OR ({r})::text = 'NaN' THEN true ELSE ({l}) <> ({r}) END)");
            }
            if matches!(
                op,
                BinaryOperator::Lt | BinaryOperator::Le | BinaryOperator::Gt | BinaryOperator::Ge
            ) {
                let o = match op {
                    BinaryOperator::Lt => "<",
                    BinaryOperator::Le => "<=",
                    BinaryOperator::Gt => ">",
                    BinaryOperator::Ge => ">=",
                    _ => "=", // fallback; outer guard already filters
                };
                return format!("(CASE WHEN ({l})::text = 'NaN' OR ({r})::text = 'NaN' THEN false ELSE ({l}) {o} ({r}) END)");
            }
            let o = match op {
                BinaryOperator::Add => "+",
                BinaryOperator::Sub => "-",
                BinaryOperator::Mul => "*",
                BinaryOperator::Div => "/",
                BinaryOperator::Mod => "%",
                BinaryOperator::Eq => "=",
                BinaryOperator::Ne => "<>",
                BinaryOperator::Lt => "<",
                BinaryOperator::Le => "<=",
                BinaryOperator::Gt => ">",
                BinaryOperator::Ge => ">=",
                BinaryOperator::And => "AND",
                BinaryOperator::Or => "OR",
                BinaryOperator::Concat => "||",
                BinaryOperator::BitwiseAnd => "&",
                BinaryOperator::BitwiseOr => "|",
                BinaryOperator::ShiftLeft => "<<",
                BinaryOperator::ShiftRight => ">>",
                BinaryOperator::Exp => "^",
                BinaryOperator::RegexMatch => "~",
                BinaryOperator::RegexMatchInsensitive => "~*",
                BinaryOperator::NotRegexMatch => "!~",
                BinaryOperator::NotRegexMatchInsensitive => "!~*",
                BinaryOperator::JsonGet => "->",
                BinaryOperator::JsonGetText => "->>",
                BinaryOperator::JsonPathGet => "#>",
                BinaryOperator::JsonPathGetText => "#>>",
                BinaryOperator::JsonContains | BinaryOperator::JsonContainedBy => "@>",
                BinaryOperator::JsonKeyExists => "?",
                BinaryOperator::JsonAnyKeyExists => "?|",
                BinaryOperator::JsonAllKeysExist => "?&",
                BinaryOperator::ArrayOverlap => "&&",
                BinaryOperator::FullTextSearch => "@@",
                BinaryOperator::JsonPathExists => "@?",
                BinaryOperator::GeometricEq => "~=",
                BinaryOperator::VectorL2Distance => "<->",
                BinaryOperator::VectorCosineDistance => "<=>",
                BinaryOperator::VectorNegativeInnerProduct => "<#>",
                BinaryOperator::VectorL1Distance => "<+>",
                BinaryOperator::VectorHammingDistance => "<~>",
                BinaryOperator::VectorJaccardDistance => "<%>",
                BinaryOperator::BitwiseXor => {
                    return format!("(({l} OR {r}) AND NOT ({l} AND {r}))");
                }
            };
            format!("({l} {o} {r})")
        }
        Expr::IsNull { expr, negated, .. } => {
            let inner = expr_to_sql_plain(expr);
            if *negated {
                format!("(({inner}) IS NOT NULL)")
            } else {
                format!("(({inner}) IS NULL)")
            }
        }
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "({} IS NOT DISTINCT FROM {})",
                    expr_to_sql_plain(left),
                    expr_to_sql_plain(right)
                )
            } else {
                format!(
                    "({} IS DISTINCT FROM {})",
                    expr_to_sql_plain(left),
                    expr_to_sql_plain(right)
                )
            }
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let not = if *negated { "NOT " } else { "" };
            let op = if *case_insensitive { "ILIKE" } else { "LIKE" };
            format!(
                "({} {not}{op} {})",
                expr_to_sql_plain(expr),
                expr_to_sql_plain(pattern)
            )
        }
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => translate_function_call(name, args, *distinct, filter.as_deref()),
        Expr::Cast {
            expr, data_type, ..
        } => format!("CAST({} AS {})", expr_to_sql_plain(expr), data_type),
        Expr::InList {
            expr,
            list,
            negated,
            ..
        } => {
            let not = if *negated { "NOT " } else { "" };
            let l: Vec<String> = list.iter().map(expr_to_sql_plain).collect();
            format!("({} {not}IN ({}))", expr_to_sql_plain(expr), l.join(", "))
        }
        Expr::Between {
            expr,
            low,
            high,
            negated,
            ..
        } => {
            let not = if *negated { "NOT " } else { "" };
            format!(
                "({} {not}BETWEEN {} AND {})",
                expr_to_sql_plain(expr),
                expr_to_sql_plain(low),
                expr_to_sql_plain(high)
            )
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut sql = String::from("CASE");
            if let Some(op) = operand {
                sql.push(' ');
                sql.push_str(&expr_to_sql_plain(op));
            }
            for (c, r) in conditions.iter().zip(results.iter()) {
                sql.push_str(" WHEN ");
                sql.push_str(&expr_to_sql_plain(c));
                sql.push_str(" THEN ");
                sql.push_str(&expr_to_sql_plain(r));
            }
            if let Some(el) = else_result {
                sql.push_str(" ELSE ");
                sql.push_str(&expr_to_sql_plain(el));
            }
            sql.push_str(" END");
            sql
        }
        Expr::Array { elements, .. } => {
            let e: Vec<String> = elements.iter().map(expr_to_sql_plain).collect();
            if e.is_empty() {
                "ARRAY[]::bigint[]".into()
            } else {
                format!("ARRAY[{}]", e.join(", "))
            }
        }
        Expr::Subquery { .. } | Expr::ArraySubquery { .. } | Expr::InSubquery { .. } => {
            "NULL".into()
        }
        Expr::Exists { negated, .. } => {
            if *negated {
                "NOT EXISTS (SELECT 1)".into()
            } else {
                "EXISTS (SELECT 1)".into()
            }
        }
        Expr::CypherExists { negated, .. } => {
            if *negated {
                "NOT EXISTS (SELECT 1)".into()
            } else {
                "EXISTS (SELECT 1)".into()
            }
        }
        Expr::CypherPatternComprehension { .. } => {
            UNSUPPORTED_CYPHER_PATTERN_COMPREHENSION_SENTINEL.into()
        }
        Expr::WindowFunction { .. } => "NULL".into(),
    }
}
