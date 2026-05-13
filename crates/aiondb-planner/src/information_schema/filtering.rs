#![allow(clippy::map_unwrap_or)]

use std::cmp::Ordering;

use aiondb_eval::compare_runtime_values;
use aiondb_parser::BinaryOperator;

use super::query_helpers::{evaluate_binary_value, find_column_index, literal_to_value};
use super::*;

pub(super) fn apply_selection(
    fields: &[ResultField],
    rows: Vec<Vec<Value>>,
    selection: Option<&Expr>,
) -> DbResult<Vec<Vec<Value>>> {
    rows.into_iter()
        .filter_map(|row| match row_matches_selection(fields, &row, selection) {
            Ok(true) => Some(Ok(row)),
            Ok(false) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

pub(super) fn row_matches_selection(
    fields: &[ResultField],
    row: &[Value],
    selection: Option<&Expr>,
) -> DbResult<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    match selection {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
            ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            Ok(compare_runtime_values(&left, &right)?
                .is_some_and(|ordering| ordering == Ordering::Equal))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
            ..
        } => Ok(row_matches_selection(fields, row, Some(left))?
            && row_matches_selection(fields, row, Some(right))?),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Or,
            right,
            ..
        } => Ok(row_matches_selection(fields, row, Some(left))?
            || row_matches_selection(fields, row, Some(right))?),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Ne,
            right,
            ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            Ok(compare_runtime_values(&left, &right)?
                .is_some_and(|ordering| ordering != Ordering::Equal))
        }
        Expr::BinaryOp {
            left,
            op:
                op @ (BinaryOperator::Lt | BinaryOperator::Le | BinaryOperator::Gt | BinaryOperator::Ge),
            right,
            ..
        } => {
            let l = resolve_value(fields, row, left)?;
            let r = resolve_value(fields, row, right)?;
            let ordering = compare_runtime_values(&l, &r)?;
            match op {
                BinaryOperator::Lt => {
                    Ok(ordering.is_some_and(|ordering| ordering == Ordering::Less))
                }
                BinaryOperator::Le => {
                    Ok(ordering.is_some_and(|ordering| ordering != Ordering::Greater))
                }
                BinaryOperator::Gt => {
                    Ok(ordering.is_some_and(|ordering| ordering == Ordering::Greater))
                }
                BinaryOperator::Ge => {
                    Ok(ordering.is_some_and(|ordering| ordering != Ordering::Less))
                }
                _ => Err(DbError::internal(format!(
                    "unsupported comparison operator in information_schema filter: {op:?}"
                ))),
            }
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            ..
        } => {
            let val = resolve_value(fields, row, expr)?;
            let pat = resolve_value(fields, row, pattern)?;
            match (&val, &pat) {
                (Value::Text(v), Value::Text(p)) => {
                    let matched = like_match(v, p);
                    Ok(if *negated { !matched } else { matched })
                }
                _ => Ok(false),
            }
        }
        Expr::IsNull { expr, negated, .. } => {
            let is_null = matches!(resolve_value(fields, row, expr)?, Value::Null);
            Ok(if *negated { !is_null } else { is_null })
        }
        Expr::UnaryOp {
            op: aiondb_parser::UnaryOperator::Not,
            expr,
            ..
        } => Ok(!row_matches_selection(fields, row, Some(expr))?),
        Expr::InList {
            expr,
            list,
            negated,
            ..
        } => {
            let val = resolve_value(fields, row, expr)?;
            let found = list.iter().any(|e| {
                resolve_value(fields, row, e)
                    .and_then(|v| compare_runtime_values(&v, &val))
                    .map(|ordering| ordering.is_some_and(|ordering| ordering == Ordering::Equal))
                    .unwrap_or(false)
            });
            Ok(if *negated { !found } else { found })
        }
        _ => {
            // matching all rows, which would return incorrect results.
            Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "unsupported WHERE clause expression on information_schema virtual table",
            ))
        }
    }
}

/// Simple SQL LIKE matching: `%` matches any sequence, `_` matches one char.
///
/// Uses an O(n*m) dynamic-programming approach instead of recursive
/// backtracking to prevent ReDoS from pathological patterns such as
/// `%a%a%a%a%b`.
pub(super) fn like_match(value: &str, pattern: &str) -> bool {
    let v: Vec<char> = value.chars().collect();

    // Parse pattern into tokens, handling backslash escapes.
    enum Pat {
        Percent,
        Underscore,
        Lit(char),
    }
    let mut pats = Vec::new();
    let mut it = pattern.chars();
    while let Some(ch) = it.next() {
        match ch {
            '%' => pats.push(Pat::Percent),
            '_' => pats.push(Pat::Underscore),
            '\\' => {
                if let Some(e) = it.next() {
                    pats.push(Pat::Lit(e));
                }
            }
            c => pats.push(Pat::Lit(c)),
        }
    }
    let m = pats.len();

    // prev[j] = "value[0..i] matches pats[0..j]"
    let mut prev = vec![false; m + 1];
    prev[0] = true;
    for (j, p) in pats.iter().enumerate() {
        if matches!(p, Pat::Percent) {
            prev[j + 1] = prev[j];
        }
    }

    for vc in &v {
        let mut curr = vec![false; m + 1];
        for (j, p) in pats.iter().enumerate() {
            curr[j + 1] = match p {
                Pat::Percent => curr[j] || prev[j + 1],
                Pat::Underscore => prev[j],
                Pat::Lit(c) => prev[j] && *vc == *c,
            };
        }
        prev = curr;
    }

    prev[m]
}

pub(super) fn resolve_value(fields: &[ResultField], row: &[Value], expr: &Expr) -> DbResult<Value> {
    match expr {
        Expr::Identifier(name) => {
            let column_name = name.parts.last().ok_or_else(|| {
                DbError::bind_error(SqlState::UndefinedColumn, "empty identifier is not allowed")
            })?;
            let index = find_column_index(fields, column_name)?;
            Ok(row[index].clone())
        }
        Expr::Literal(literal, _) => Ok(literal_to_value(literal)),
        // Handle 'value'::type casts - strip the cast and resolve the inner
        Expr::Cast { expr, .. } => resolve_value(fields, row, expr),
        Expr::FunctionCall { name, args, .. }
            if name.parts.last().is_some_and(|part| {
                part.eq_ignore_ascii_case("__aiondb_type_hint")
                    || part.eq_ignore_ascii_case("__aiondb_char_pad_length")
            }) && !args.is_empty() =>
        {
            resolve_value(fields, row, &args[0])
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            evaluate_binary_value(*op, left, right)
        }
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "unsupported expression on information_schema virtual table",
        )),
    }
}
