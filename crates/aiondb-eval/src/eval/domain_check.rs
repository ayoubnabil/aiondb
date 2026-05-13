//! Lightweight evaluator for domain CHECK constraint expressions.
//!
//! Domain CHECK expressions use `VALUE` as a placeholder for the value being
//! validated.  This module parses the expression text using `aiondb_parser`,
//! substitutes the concrete value for `VALUE`, and evaluates the expression
//! to a boolean.

use std::cmp::Ordering;

use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState, Value};
use aiondb_parser::{BinaryOperator, Expr, Literal, UnaryOperator};

use crate::eval::scalar_functions::value_convert::to_i32_saturating;
use crate::eval::session::{normalize_compat_type_name, with_current_session_context};

const MAX_DOMAIN_CHECK_EXPR_DEPTH: usize = 256;

/// Validate a value against all constraints of a domain (and its parent
/// domains in the chain).  Returns `Ok(())` if valid, or an appropriate
/// `DbError` on the first violation.
pub fn enforce_domain_constraints(value: &Value, domain_name: &str) -> DbResult<()> {
    let normalized = normalize_compat_type_name(domain_name);

    with_current_session_context(|ctx| {
        // Walk the domain chain from the target domain up to the base type,
        // collecting and checking constraints at each level.
        let mut current_domain = normalized;
        for _ in 0..32 {
            let def = match ctx.domain_def(&current_domain) {
                Some(d) => d.clone(),
                None => break,
            };

            // Check NOT NULL constraint
            if def.not_null && value.is_null() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NotNullViolation,
                    format!("domain {} does not allow null values", def.name),
                )));
            }

            // Check each CHECK constraint
            if !value.is_null() {
                for constraint in &def.constraints {
                    // Skip "VALUE IS NOT NULL" constraints - handled above
                    if constraint
                        .check_expr
                        .trim()
                        .eq_ignore_ascii_case("VALUE IS NOT NULL")
                    {
                        continue;
                    }

                    match eval_domain_check_expr(&constraint.check_expr, value) {
                        Ok(Some(true) | None) => {
                            // Constraint satisfied (TRUE or NULL both pass)
                        }
                        Ok(Some(false)) => {
                            return Err(DbError::from_report(ErrorReport::new(
                                SqlState::CheckViolation,
                                format!(
                                    "value for domain {} violates check constraint \"{}\"",
                                    def.name, constraint.name
                                ),
                            )));
                        }
                        Err(e) => {
                            // If the expression evaluation itself errors (e.g. division by zero),
                            // propagate that error directly.
                            return Err(e);
                        }
                    }
                }
            }

            current_domain = normalize_compat_type_name(&def.base_type);
        }

        Ok(())
    })
}

/// Evaluate a domain CHECK expression with a concrete value substituted for
/// `VALUE`.  Returns `Ok(Some(bool))` for a definite result, `Ok(None)` for
/// NULL (which satisfies CHECK), or `Err` for evaluation errors.
fn eval_domain_check_expr(expr_text: &str, value: &Value) -> DbResult<Option<bool>> {
    let parsed = aiondb_parser::parse_expression(expr_text).map_err(|e| {
        DbError::internal(format!(
            "failed to parse domain CHECK expression: {expr_text}: {e}"
        ))
    })?;

    let result = eval_expr(&parsed, value)?;
    match result {
        Value::Boolean(b) => Ok(Some(b)),
        Value::Null => Ok(None),
        _ => Err(DbError::internal(
            "domain CHECK expression did not evaluate to BOOLEAN",
        )),
    }
}

fn eval_expr(expr: &Expr, value: &Value) -> DbResult<Value> {
    eval_expr_at_depth(expr, value, 0)
}

/// Evaluate a parsed expression, substituting `value` for any identifier named
/// "value" (case-insensitive), with a hard depth cap for generated checks.
fn eval_expr_at_depth(expr: &Expr, value: &Value, depth: usize) -> DbResult<Value> {
    if depth >= MAX_DOMAIN_CHECK_EXPR_DEPTH {
        return Err(DbError::program_limit(format!(
            "domain CHECK expression depth exceeds limit {MAX_DOMAIN_CHECK_EXPR_DEPTH}"
        )));
    }
    match expr {
        Expr::Identifier(name) => {
            let ident = name.parts.last().map_or("", String::as_str);
            if ident.eq_ignore_ascii_case("value") {
                Ok(value.clone())
            } else {
                Err(DbError::internal(format!(
                    "unknown identifier in domain CHECK: {ident}"
                )))
            }
        }
        Expr::Literal(Literal::Integer(v), _) => {
            if let Ok(i) = i32::try_from(*v) {
                Ok(Value::Int(i))
            } else {
                Ok(Value::BigInt(*v))
            }
        }
        Expr::Literal(Literal::NumericLit(v), _) => {
            v.parse::<NumericValue>().map(Value::Numeric).map_err(|e| {
                DbError::internal(format!("invalid numeric literal in domain CHECK: {e}"))
            })
        }
        Expr::Literal(Literal::String(v), _) => Ok(Value::Text(v.clone())),
        Expr::Literal(Literal::Boolean(v), _) => Ok(Value::Boolean(*v)),
        Expr::Literal(Literal::Null, _) => Ok(Value::Null),
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left_val = eval_expr_at_depth(left, value, depth + 1)?;
            let right_val = eval_expr_at_depth(right, value, depth + 1)?;
            eval_binary_op(&left_val, op, &right_val)
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: inner,
            ..
        } => {
            let inner_val = eval_expr_at_depth(inner, value, depth + 1)?;
            match inner_val {
                Value::Boolean(b) => Ok(Value::Boolean(!b)),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::internal("NOT applied to non-boolean")),
            }
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
            ..
        } => {
            let inner_val = eval_expr_at_depth(inner, value, depth + 1)?;
            match inner_val {
                Value::Int(v) => v
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or_else(|| DbError::internal("integer out of range")),
                Value::BigInt(v) => v
                    .checked_neg()
                    .map(Value::BigInt)
                    .ok_or_else(|| DbError::internal("bigint out of range")),
                Value::Double(v) => Ok(Value::Double(-v)),
                Value::Numeric(ref nv) => Ok(Value::Numeric(nv.neg())),
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::internal("unary minus on unsupported type")),
            }
        }
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            let inner_val = eval_expr_at_depth(inner, value, depth + 1)?;
            let is_null = inner_val.is_null();
            Ok(Value::Boolean(if *negated { !is_null } else { is_null }))
        }
        Expr::FunctionCall { name, args, .. } => {
            let func_name = name
                .parts
                .last()
                .map_or("", String::as_str)
                .to_ascii_lowercase();
            eval_function_call(&func_name, args, value, depth + 1)
        }
        Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => {
            let inner_val = eval_expr_at_depth(inner, value, depth + 1)?;
            if inner_val.is_null() {
                return Ok(Value::Null);
            }
            let mut found = false;
            for item in list {
                let item_val = eval_expr_at_depth(item, value, depth + 1)?;
                if let Some(Ordering::Equal) = compare_values_simple(&inner_val, &item_val)? {
                    found = true;
                    break;
                }
            }
            Ok(Value::Boolean(if *negated { !found } else { found }))
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => {
            let val = eval_expr_at_depth(inner, value, depth + 1)?;
            let low_val = eval_expr_at_depth(low, value, depth + 1)?;
            let high_val = eval_expr_at_depth(high, value, depth + 1)?;
            if val.is_null() || low_val.is_null() || high_val.is_null() {
                return Ok(Value::Null);
            }
            let ge_low = matches!(
                compare_values_simple(&val, &low_val)?,
                Some(Ordering::Equal | Ordering::Greater)
            );
            let le_high = matches!(
                compare_values_simple(&val, &high_val)?,
                Some(Ordering::Equal | Ordering::Less)
            );
            let between = ge_low && le_high;
            Ok(Value::Boolean(if *negated { !between } else { between }))
        }
        _ => Err(DbError::internal(format!(
            "unsupported expression in domain CHECK: {expr:?}"
        ))),
    }
}

fn compare_values_simple(left: &Value, right: &Value) -> DbResult<Option<Ordering>> {
    super::operators::compare_runtime_values(left, right)
}

fn eval_binary_op(left: &Value, op: &BinaryOperator, right: &Value) -> DbResult<Value> {
    // Handle NULL propagation for most operators
    if left.is_null() || right.is_null() {
        return match op {
            BinaryOperator::And => match (left, right) {
                (Value::Boolean(false), _) | (_, Value::Boolean(false)) => {
                    Ok(Value::Boolean(false))
                }
                _ => Ok(Value::Null),
            },
            BinaryOperator::Or => match (left, right) {
                (Value::Boolean(true), _) | (_, Value::Boolean(true)) => Ok(Value::Boolean(true)),
                _ => Ok(Value::Null),
            },
            _ => Ok(Value::Null),
        };
    }

    match op {
        BinaryOperator::Eq => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(cmp == Some(Ordering::Equal)))
        }
        BinaryOperator::Ne => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(cmp != Some(Ordering::Equal)))
        }
        BinaryOperator::Lt => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(cmp == Some(Ordering::Less)))
        }
        BinaryOperator::Le => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(matches!(
                cmp,
                Some(Ordering::Less | Ordering::Equal)
            )))
        }
        BinaryOperator::Gt => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(cmp == Some(Ordering::Greater)))
        }
        BinaryOperator::Ge => {
            let cmp = compare_values_simple(left, right)?;
            Ok(Value::Boolean(matches!(
                cmp,
                Some(Ordering::Greater | Ordering::Equal)
            )))
        }
        BinaryOperator::And => match (left, right) {
            (Value::Boolean(a), Value::Boolean(b)) => Ok(Value::Boolean(*a && *b)),
            _ => Ok(Value::Null),
        },
        BinaryOperator::Or => match (left, right) {
            (Value::Boolean(a), Value::Boolean(b)) => Ok(Value::Boolean(*a || *b)),
            _ => Ok(Value::Null),
        },
        BinaryOperator::Add => arith_add(left, right),
        BinaryOperator::Sub => arith_sub(left, right),
        BinaryOperator::Mul => arith_mul(left, right),
        BinaryOperator::Div => arith_div(left, right),
        BinaryOperator::Mod => arith_mod(left, right),
        _ => Err(DbError::internal(format!(
            "unsupported operator in domain CHECK: {op:?}"
        ))),
    }
}

// Inline arithmetic for domain CHECK evaluation (to avoid pub(super) visibility issues)
fn arith_add(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => a
            .checked_add(*b)
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_add(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a + b)),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(*a as f64 + b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a + *b as f64)),
        (Value::Numeric(a), Value::Numeric(b)) => Ok(Value::Numeric(a.add(b))),
        _ => Err(DbError::internal("unsupported types for addition")),
    }
}

fn arith_sub(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => a
            .checked_sub(*b)
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_sub(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a - b)),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(*a as f64 - b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a - *b as f64)),
        _ => Err(DbError::internal("unsupported types for subtraction")),
    }
}

fn arith_mul(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => a
            .checked_mul(*b)
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_mul(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a * b)),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(*a as f64 * b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a * *b as f64)),
        _ => Err(DbError::internal("unsupported types for multiplication")),
    }
}

fn arith_div(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_div(*b)
                .map(Value::Int)
                .ok_or_else(|| DbError::internal("integer out of range"))
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_div(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("bigint out of range"))
        }
        // IEEE-754: float8/float4 divide-by-zero yields ±Infinity (or NaN
        // for 0/0). Only integer and numeric raise. PG follows the same
        // distinction; whenever either side is Double the operation is
        // promoted to float and produces an IEEE result, never an error.
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a / b)),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(*a as f64 / b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a / *b as f64)),
        _ => Err(DbError::internal("unsupported types for division")),
    }
}

fn arith_mod(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_rem(*b)
                .map(Value::Int)
                .ok_or_else(|| DbError::internal("integer out of range"))
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_rem(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("bigint out of range"))
        }
        _ => Err(DbError::internal("unsupported types for modulo")),
    }
}

fn eval_function_call(name: &str, args: &[Expr], value: &Value, depth: usize) -> DbResult<Value> {
    match name {
        "upper" => {
            if args.len() != 1 {
                return Err(DbError::internal("upper requires exactly 1 argument"));
            }
            let val = eval_expr_at_depth(&args[0], value, depth + 1)?;
            match val {
                Value::Text(text) => {
                    let Some(upper) = extract_range_upper_bound_i64(&text) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::BigInt(upper))
                }
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::internal(
                    "upper argument must be range or multirange text",
                )),
            }
        }
        "substring" | "substr" => {
            if args.len() < 2 {
                return Err(DbError::internal("substring requires at least 2 arguments"));
            }
            let string_val = eval_expr_at_depth(&args[0], value, depth + 1)?;
            let from_val = eval_expr_at_depth(&args[1], value, depth + 1)?;
            let for_val = if args.len() > 2 {
                Some(eval_expr_at_depth(&args[2], value, depth + 1)?)
            } else {
                None
            };
            let text = match string_val {
                Value::Text(s) => s,
                Value::Null => return Ok(Value::Null),
                _ => return Err(DbError::internal("substring first arg must be text")),
            };
            let from = match from_val {
                Value::Int(i) if i >= 0 => usize::try_from(i).unwrap_or(usize::MAX),
                Value::BigInt(i) if i >= 0 => usize::try_from(i).unwrap_or(usize::MAX),
                Value::Int(_) | Value::BigInt(_) => 0,
                _ => return Err(DbError::internal("substring from must be integer")),
            };
            let from_idx = from.saturating_sub(1); // SQL is 1-based
                                                   // Stream chars instead of materialising a Vec<char>.
            let result = if let Some(for_val) = for_val {
                let len = match for_val {
                    Value::Int(i) if i >= 0 => usize::try_from(i).unwrap_or(usize::MAX),
                    Value::BigInt(i) if i >= 0 => usize::try_from(i).unwrap_or(usize::MAX),
                    Value::Int(_) | Value::BigInt(_) => 0,
                    _ => return Err(DbError::internal("substring for must be integer")),
                };
                text.chars().skip(from_idx).take(len).collect::<String>()
            } else {
                text.chars().skip(from_idx).collect::<String>()
            };
            Ok(Value::Text(result))
        }
        "length" | "char_length" | "character_length" => {
            if args.is_empty() {
                return Err(DbError::internal("length requires 1 argument"));
            }
            let val = eval_expr_at_depth(&args[0], value, depth + 1)?;
            match val {
                Value::Text(s) => {
                    // ASCII fast path: byte length == char count.
                    let count = if s.is_ascii() {
                        s.len()
                    } else {
                        s.chars().count()
                    };
                    Ok(Value::Int(to_i32_saturating(count)))
                }
                Value::Null => Ok(Value::Null),
                _ => Err(DbError::internal("length argument must be text")),
            }
        }
        _ => Err(DbError::internal(format!(
            "unsupported function in domain CHECK: {name}"
        ))),
    }
}

fn extract_range_upper_bound_i64(input: &str) -> Option<i64> {
    let text = input.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("empty") {
        return None;
    }

    let inner = if text.starts_with('{') && text.ends_with('}') && text.len() >= 2 {
        &text[1..text.len().saturating_sub(1)]
    } else {
        text
    };
    let upper_part = inner.rsplit(',').next()?.trim();

    let mut end = 0usize;
    for (idx, ch) in upper_part.char_indices() {
        if (idx == 0 && (ch == '-' || ch == '+')) || ch.is_ascii_digit() {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }

    upper_part.get(..end)?.parse::<i64>().ok()
}
