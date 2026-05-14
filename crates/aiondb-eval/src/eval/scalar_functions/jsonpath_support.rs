use std::borrow::Cow;

use serde_json::Value as JV;

use super::{eval_expr, CmpOp, EvalCtx, FilterExpr, JsonPathValue};

/// Hard cap on JSON recursion depth to prevent stack overflow on deeply nested values.
const MAX_JSON_RECURSE_DEPTH: u32 = 128;

pub(super) fn collect_recursive<'a>(
    v: &'a JV,
    depth: u32,
    min: u32,
    max: u32,
    out: &mut Vec<JsonPathValue<'a>>,
) {
    if depth >= min && depth <= max {
        out.push(Cow::Borrowed(v));
    }
    if depth < max && depth < MAX_JSON_RECURSE_DEPTH {
        match v {
            JV::Object(map) => {
                for child in map.values() {
                    collect_recursive(child, depth + 1, min, max, out);
                }
            }
            JV::Array(arr) => {
                for child in arr {
                    collect_recursive(child, depth + 1, min, max, out);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn collect_recursive_owned(
    v: &JV,
    depth: u32,
    min: u32,
    max: u32,
    out: &mut Vec<JsonPathValue<'_>>,
) {
    if depth >= min && depth <= max {
        out.push(Cow::Owned(v.clone()));
    }
    if depth < max && depth < MAX_JSON_RECURSE_DEPTH {
        match v {
            JV::Object(map) => {
                for child in map.values() {
                    collect_recursive_owned(child, depth + 1, min, max, out);
                }
            }
            JV::Array(arr) => {
                for child in arr {
                    collect_recursive_owned(child, depth + 1, min, max, out);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn eval_filter(filter: &FilterExpr, current: &JV, ctx: &EvalCtx) -> Option<bool> {
    match filter {
        FilterExpr::Comparison(op, left, right) => {
            let left_vals = eval_expr(left, current, ctx);
            let right_vals = eval_expr(right, current, ctx);
            // In lax mode, comparison is true if ANY left-right pair matches
            for lv in &left_vals {
                for rv in &right_vals {
                    if let Some(result) = compare_json(lv.as_ref(), rv.as_ref(), *op) {
                        if result {
                            return Some(true);
                        }
                    }
                }
            }
            // If we had values but none matched
            if !left_vals.is_empty() && !right_vals.is_empty() {
                Some(false)
            } else {
                // No values - comparison result is unknown
                None
            }
        }
        FilterExpr::And(left, right) => {
            let l = eval_filter(left, current, ctx);
            let r = eval_filter(right, current, ctx);
            match (l, r) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            }
        }
        FilterExpr::Or(left, right) => {
            let l = eval_filter(left, current, ctx);
            let r = eval_filter(right, current, ctx);
            match (l, r) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            }
        }
        FilterExpr::Not(inner) => eval_filter(inner, current, ctx).map(|b| !b),
        FilterExpr::Exists(expr) => {
            let vals = eval_expr(expr, current, ctx);
            Some(!vals.is_empty())
        }
        FilterExpr::IsUnknown(inner) => {
            let result = eval_filter(inner, current, ctx);
            Some(result.is_none())
        }
        FilterExpr::PathPredicate(expr) => {
            let vals = eval_expr(expr, current, ctx);
            match vals.first() {
                Some(v) => match v.as_ref() {
                    JV::Bool(b) => Some(*b),
                    _ => Some(!vals.is_empty()),
                },
                None => Some(false),
            }
        }
    }
}

pub(super) fn compare_json(left: &JV, right: &JV, op: CmpOp) -> Option<bool> {
    // Same-type comparisons
    match (left, right) {
        (JV::Null, JV::Null) => {
            let result = match op {
                CmpOp::Eq => true,
                CmpOp::Ne => false,
                CmpOp::Le | CmpOp::Ge => true,
                CmpOp::Lt | CmpOp::Gt => false,
            };
            Some(result)
        }
        (JV::Null, _) | (_, JV::Null) => {
            // Comparison with null is unknown in strict SQL/JSON semantics,
            // but PG treats it as: null != anything => true, etc.
            match op {
                CmpOp::Eq => Some(false),
                CmpOp::Ne => Some(true),
                _ => None,
            }
        }
        (JV::Bool(a), JV::Bool(b)) => {
            let ai = i32::from(*a);
            let bi = i32::from(*b);
            Some(apply_cmp_i(ai, bi, op))
        }
        (JV::Number(a), JV::Number(b)) => {
            let af = a.as_f64()?;
            let bf = b.as_f64()?;
            Some(apply_cmp_f(af, bf, op))
        }
        (JV::String(a), JV::String(b)) => Some(apply_cmp_s(a, b, op)),
        // Cross-type: number vs string (PG returns unknown/false)
        _ => None,
    }
}

fn apply_cmp_f(a: f64, b: f64, op: CmpOp) -> bool {
    match op {
        CmpOp::Eq => (a - b).abs() < f64::EPSILON,
        CmpOp::Ne => (a - b).abs() >= f64::EPSILON,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
    }
}

fn apply_cmp_i(a: i32, b: i32, op: CmpOp) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
    }
}

fn apply_cmp_s(a: &str, b: &str, op: CmpOp) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
    }
}
