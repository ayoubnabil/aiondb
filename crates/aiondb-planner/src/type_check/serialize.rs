use aiondb_core::{DbError, DbResult, ErrorReport, SqlState};
use aiondb_parser::{BinaryOperator, Expr, Literal, UnaryOperator};

use super::support::display_function_name;

fn literal_text(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Literal(Literal::String(value), _) => Some(value.as_str()),
        _ => None,
    }
}

fn serialize_is_json_predicate(args: &[Expr]) -> DbResult<Option<String>> {
    if args.len() != 3 {
        return Ok(None);
    }
    let input = serialize_expr(&args[0])?;
    let Some(kind) = literal_text(&args[1]).map(str::to_ascii_uppercase) else {
        return Ok(None);
    };
    let Some(unique_mode) = literal_text(&args[2]).map(str::to_ascii_uppercase) else {
        return Ok(None);
    };

    let mut rendered = format!("{input} IS JSON");
    match kind.as_str() {
        "JSON" | "VALUE" => {}
        "OBJECT" => rendered.push_str(" OBJECT"),
        "ARRAY" => rendered.push_str(" ARRAY"),
        "SCALAR" => rendered.push_str(" SCALAR"),
        _ => return Ok(None),
    }
    match unique_mode.as_str() {
        "DEFAULT" => {}
        "WITH" => rendered.push_str(" WITH UNIQUE KEYS"),
        "WITHOUT" => rendered.push_str(" WITHOUT UNIQUE KEYS"),
        _ => return Ok(None),
    }
    Ok(Some(rendered))
}

pub(crate) fn serialize_expr(expr: &Expr) -> DbResult<String> {
    match expr {
        Expr::Literal(Literal::Integer(value), _) => Ok(value.to_string()),
        Expr::Literal(Literal::NumericLit(value), _) => Ok(value.clone()),
        Expr::Literal(Literal::String(value), _) => Ok(format!("'{}'", value.replace('\'', "''"))),
        Expr::Literal(Literal::Boolean(true), _) => Ok("TRUE".to_owned()),
        Expr::Literal(Literal::Boolean(false), _) => Ok("FALSE".to_owned()),
        Expr::Literal(Literal::Null, _) => Ok("NULL".to_owned()),
        Expr::Identifier(name) => Ok(name.parts.join(".")),
        Expr::Parameter { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain parameters",
            )
            .with_position(span.start + 1),
        ))),
        Expr::Default { span } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain DEFAULT",
            )
            .with_position(span.start + 1),
        ))),
        Expr::FunctionCall { name, args, .. } => {
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_is_json"))
            {
                if let Some(rendered) = serialize_is_json_predicate(args)? {
                    return Ok(rendered);
                }
            }
            Ok(format!(
                "{}({})",
                display_function_name(&name.parts.join(".")),
                args.iter()
                    .map(serialize_expr)
                    .collect::<DbResult<Vec<_>>>()?
                    .join(", ")
            ))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
            ..
        } => Ok(format!("NOT {}", serialize_expr(expr)?)),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
            ..
        } => Ok(format!("(-{})", serialize_expr(expr)?)),
        Expr::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr,
            ..
        } => Ok(format!("(~{})", serialize_expr(expr)?)),
        Expr::UnaryOp {
            op: UnaryOperator::Abs,
            expr,
            ..
        } => Ok(format!("(@{})", serialize_expr(expr)?)),
        Expr::UnaryOp {
            op: UnaryOperator::SquareRoot,
            expr,
            ..
        } => Ok(format!("(|/{})", serialize_expr(expr)?)),
        Expr::UnaryOp {
            op: UnaryOperator::CubeRoot,
            expr,
            ..
        } => Ok(format!("(||/{})", serialize_expr(expr)?)),
        Expr::BinaryOp {
            left, op, right, ..
        } => Ok(format!(
            "{} {} {}",
            serialize_expr(left)?,
            match op {
                BinaryOperator::Add => "+",
                BinaryOperator::And => "AND",
                BinaryOperator::Concat => "||",
                BinaryOperator::Div => "/",
                BinaryOperator::Eq => "=",
                BinaryOperator::Ge => ">=",
                BinaryOperator::Gt => ">",
                BinaryOperator::Le => "<=",
                BinaryOperator::Lt => "<",
                BinaryOperator::Mod => "%",
                BinaryOperator::Mul => "*",
                BinaryOperator::Ne => "!=",
                BinaryOperator::Or => "OR",
                BinaryOperator::Sub => "-",
                BinaryOperator::JsonGet => "->",
                BinaryOperator::JsonGetText => "->>",
                BinaryOperator::JsonPathGet => "#>",
                BinaryOperator::JsonPathGetText => "#>>",
                BinaryOperator::JsonContains => "@>",
                BinaryOperator::JsonContainedBy => "<@",
                BinaryOperator::JsonKeyExists => "?",
                BinaryOperator::JsonAnyKeyExists => "?|",
                BinaryOperator::JsonAllKeysExist => "?&",
                BinaryOperator::ArrayOverlap => "&&",
                BinaryOperator::Exp => "^",
                BinaryOperator::BitwiseAnd => "&",
                BinaryOperator::BitwiseOr => "|",
                BinaryOperator::BitwiseXor => "#",
                BinaryOperator::ShiftLeft => "<<",
                BinaryOperator::ShiftRight => ">>",
                BinaryOperator::RegexMatch => "~",
                BinaryOperator::RegexMatchInsensitive => "~*",
                BinaryOperator::NotRegexMatch => "!~",
                BinaryOperator::NotRegexMatchInsensitive => "!~*",
                BinaryOperator::FullTextSearch => "@@",
                BinaryOperator::JsonPathExists => "@?",
                BinaryOperator::GeometricEq => "~=",
                BinaryOperator::VectorL2Distance => "<->",
                BinaryOperator::VectorCosineDistance => "<=>",
                BinaryOperator::VectorNegativeInnerProduct => "<#>",
                BinaryOperator::VectorL1Distance => "<+>",
                BinaryOperator::VectorHammingDistance => "<~>",
                BinaryOperator::VectorJaccardDistance => "<%>",
            },
            serialize_expr(right)?,
        )),
        Expr::IsNull {
            expr,
            negated: false,
            ..
        } => Ok(format!("{} IS NULL", serialize_expr(expr)?)),
        Expr::IsNull {
            expr,
            negated: true,
            ..
        } => Ok(format!("{} IS NOT NULL", serialize_expr(expr)?)),
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            let l = serialize_expr(left)?;
            let r = serialize_expr(right)?;
            if *negated {
                Ok(format!("{l} IS NOT DISTINCT FROM {r}"))
            } else {
                Ok(format!("{l} IS DISTINCT FROM {r}"))
            }
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let op = if *case_insensitive { "ILIKE" } else { "LIKE" };
            if *negated {
                Ok(format!(
                    "{} NOT {op} {}",
                    serialize_expr(expr)?,
                    serialize_expr(pattern)?
                ))
            } else {
                Ok(format!(
                    "{} {op} {}",
                    serialize_expr(expr)?,
                    serialize_expr(pattern)?
                ))
            }
        }
        Expr::InList {
            expr,
            list,
            negated,
            ..
        } => {
            let items = list
                .iter()
                .map(serialize_expr)
                .collect::<DbResult<Vec<_>>>()?
                .join(", ");
            let not = if *negated { " NOT" } else { "" };
            Ok(format!("{}{} IN ({})", serialize_expr(expr)?, not, items))
        }
        Expr::Between {
            expr,
            low,
            high,
            negated,
            ..
        } => {
            let not = if *negated { " NOT" } else { "" };
            Ok(format!(
                "{}{} BETWEEN {} AND {}",
                serialize_expr(expr)?,
                not,
                serialize_expr(low)?,
                serialize_expr(high)?
            ))
        }
        Expr::Cast {
            expr, data_type, ..
        } => Ok(format!("CAST({} AS {})", serialize_expr(expr)?, data_type)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut s = String::from("CASE");
            if let Some(operand) = operand {
                s.push(' ');
                s.push_str(&serialize_expr(operand)?);
            }
            // Stream each WHEN/THEN pair and the ELSE branch directly
            // into `s` instead of allocating a transient format!() per
            // pair.
            use std::fmt::Write;
            for (cond, result) in conditions.iter().zip(results.iter()) {
                let _ = write!(
                    s,
                    " WHEN {} THEN {}",
                    serialize_expr(cond)?,
                    serialize_expr(result)?
                );
            }
            if let Some(else_result) = else_result {
                let _ = write!(s, " ELSE {}", serialize_expr(else_result)?);
            }
            s.push_str(" END");
            Ok(s)
        }
        Expr::Array { elements, .. } => {
            let items = elements
                .iter()
                .map(serialize_expr)
                .collect::<DbResult<Vec<_>>>()?
                .join(", ");
            Ok(format!("ARRAY[{items}]"))
        }
        Expr::ArraySubquery { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::Subquery { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::InSubquery { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::Exists { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::CypherExists { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::CypherPatternComprehension { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain subqueries",
            )
            .with_position(span.start + 1),
        ))),
        Expr::WindowFunction { span, .. } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain window functions",
            )
            .with_position(span.start + 1),
        ))),
    }
}
