#![allow(clippy::unnested_or_patterns, clippy::doc_markdown)]

#[path = "support_expr_helpers.rs"]
mod support_expr_helpers;
#[path = "support_relation_helpers.rs"]
mod support_relation_helpers;

use aiondb_catalog::{ColumnDescriptor, TableDescriptor};
use aiondb_core::{ColumnId, DataType, DbError, DbResult, ErrorReport, SqlState, Value};
use aiondb_parser::{Expr, Literal, ObjectName};
use aiondb_plan::{TypedExpr, TypedExprKind};

pub(super) use self::support_expr_helpers::{
    default_column_name, display_function_name, expr_contains_parameter, merge_parameter_types,
};
pub(super) use self::support_relation_helpers::{
    ambiguous_column, compat_relation_with_system_columns, is_system_column,
    relation_with_alias_columns, resolve_session_variable, rewrite_table_aliases, undefined_column,
};

pub(super) fn resolve_arithmetic_type(left: &DataType, right: &DataType) -> DbResult<DataType> {
    match (left, right) {
        (DataType::Int, DataType::Int) => Ok(DataType::Int),
        (DataType::Int, DataType::BigInt) | (DataType::BigInt, DataType::Int) => {
            Ok(DataType::BigInt)
        }
        (DataType::BigInt, DataType::BigInt) => Ok(DataType::BigInt),
        (DataType::Real, DataType::Real) => Ok(DataType::Real),
        (DataType::Double, DataType::Double) => Ok(DataType::Double),
        (DataType::Int, DataType::Real) | (DataType::Real, DataType::Int) => Ok(DataType::Real),
        (DataType::Int, DataType::Double) | (DataType::Double, DataType::Int) => {
            Ok(DataType::Double)
        }
        (DataType::BigInt, DataType::Double) | (DataType::Double, DataType::BigInt) => {
            Ok(DataType::Double)
        }
        (DataType::BigInt, DataType::Real) | (DataType::Real, DataType::BigInt) => {
            Ok(DataType::Double)
        }
        (DataType::Real, DataType::Double) | (DataType::Double, DataType::Real) => {
            Ok(DataType::Double)
        }
        (DataType::Numeric, DataType::Numeric) => Ok(DataType::Numeric),
        (DataType::Int | DataType::BigInt, DataType::Numeric)
        | (DataType::Numeric, DataType::Int | DataType::BigInt) => Ok(DataType::Numeric),
        (DataType::Real | DataType::Double, DataType::Numeric)
        | (DataType::Numeric, DataType::Real | DataType::Double) => Ok(DataType::Double),
        (DataType::Money, DataType::Money) => Ok(DataType::Money),
        (DataType::Money, DataType::Int | DataType::BigInt | DataType::Real | DataType::Double)
        | (DataType::Int | DataType::BigInt | DataType::Real | DataType::Double, DataType::Money)
        | (DataType::Money, DataType::Text)
        | (DataType::Text, DataType::Money) => Ok(DataType::Money),
        (DataType::PgLsn, DataType::PgLsn) => Ok(DataType::Numeric),
        (DataType::PgLsn, DataType::Int | DataType::BigInt | DataType::Numeric)
        | (DataType::Int | DataType::BigInt | DataType::Numeric, DataType::PgLsn) => {
            Ok(DataType::PgLsn)
        }
        // Date/time arithmetic
        (DataType::Date, DataType::Int) | (DataType::Int, DataType::Date) => Ok(DataType::Date),
        (DataType::Date, DataType::BigInt) | (DataType::BigInt, DataType::Date) => {
            Ok(DataType::Date)
        }
        (DataType::Date, DataType::Date) => Ok(DataType::Int),
        (DataType::Date, DataType::Interval) | (DataType::Interval, DataType::Date) => {
            Ok(DataType::Timestamp)
        }
        (DataType::Timestamp, DataType::Interval) | (DataType::Interval, DataType::Timestamp) => {
            Ok(DataType::Timestamp)
        }
        (DataType::TimestampTz, DataType::Interval)
        | (DataType::Interval, DataType::TimestampTz) => Ok(DataType::TimestampTz),
        (DataType::Timestamp, DataType::Timestamp) => Ok(DataType::Interval),
        (DataType::TimestampTz, DataType::TimestampTz) => Ok(DataType::Interval),
        // Cross timestamp/timestamptz subtraction
        (DataType::Timestamp, DataType::TimestampTz)
        | (DataType::TimestampTz, DataType::Timestamp) => Ok(DataType::Interval),
        (DataType::Time, DataType::Interval) | (DataType::Interval, DataType::Time) => {
            Ok(DataType::Time)
        }
        (DataType::TimeTz, DataType::Interval) | (DataType::Interval, DataType::TimeTz) => {
            Ok(DataType::TimeTz)
        }
        // Time - Time → Interval (PG compat)
        (DataType::Time, DataType::Time) => Ok(DataType::Interval),
        (DataType::TimeTz, DataType::TimeTz) => Ok(DataType::Interval),
        // Date + Time → Timestamp (PG compat)
        (DataType::Date, DataType::Time) | (DataType::Time, DataType::Date) => {
            Ok(DataType::Timestamp)
        }
        (DataType::Date, DataType::TimeTz) | (DataType::TimeTz, DataType::Date) => {
            Ok(DataType::TimestampTz)
        }
        (DataType::Interval, DataType::Interval) => Ok(DataType::Interval),
        (
            DataType::Interval,
            DataType::Int
            | DataType::BigInt
            | DataType::Double
            | DataType::Numeric
            | DataType::Real,
        )
        | (
            DataType::Int
            | DataType::BigInt
            | DataType::Double
            | DataType::Numeric
            | DataType::Real,
            DataType::Interval,
        ) => Ok(DataType::Interval),
        // Boolean <-> Int arithmetic (PG treats TRUE as 1, FALSE as 0)
        (DataType::Boolean, DataType::Int) | (DataType::Int, DataType::Boolean) => {
            Ok(DataType::Int)
        }
        (DataType::Boolean, DataType::BigInt) | (DataType::BigInt, DataType::Boolean) => {
            Ok(DataType::BigInt)
        }
        (DataType::Boolean, DataType::Numeric) | (DataType::Numeric, DataType::Boolean) => {
            Ok(DataType::Numeric)
        }
        (DataType::Boolean, DataType::Double) | (DataType::Double, DataType::Boolean) => {
            Ok(DataType::Double)
        }
        (DataType::Boolean, DataType::Real) | (DataType::Real, DataType::Boolean) => {
            Ok(DataType::Real)
        }
        (DataType::Boolean, DataType::Boolean) => Ok(DataType::Int),
        // Text with numeric types: PG implicitly coerces text to numeric for arithmetic
        (DataType::Text, r) if is_numeric(r) => Ok(r.clone()),
        (l, DataType::Text) if is_numeric(l) => Ok(l.clone()),
        // PG implicitly coerces text to interval/timestamp/date/time when
        // mixed with one of those temporal types in arithmetic.
        (DataType::Text, DataType::Interval) | (DataType::Interval, DataType::Text) => {
            Ok(DataType::Interval)
        }
        (DataType::Text, DataType::Timestamp) | (DataType::Timestamp, DataType::Text) => {
            Ok(DataType::Timestamp)
        }
        (DataType::Text, DataType::TimestampTz) | (DataType::TimestampTz, DataType::Text) => {
            Ok(DataType::TimestampTz)
        }
        (DataType::Text, DataType::Date) | (DataType::Date, DataType::Text) => Ok(DataType::Date),
        (DataType::Text, DataType::Time) | (DataType::Time, DataType::Text) => Ok(DataType::Time),
        (DataType::Text, DataType::TimeTz) | (DataType::TimeTz, DataType::Text) => {
            Ok(DataType::TimeTz)
        }
        (DataType::Text, DataType::Text) => Ok(DataType::Numeric),
        // Array types with numeric: element-wise arithmetic, result is an array
        (DataType::Array(elem), r) if is_numeric(r) || is_numeric(elem) => {
            let result_elem = resolve_arithmetic_type(elem, r).unwrap_or_else(|_| *elem.clone());
            Ok(DataType::Array(Box::new(result_elem)))
        }
        (l, DataType::Array(elem)) if is_numeric(l) || is_numeric(elem) => {
            let result_elem = resolve_arithmetic_type(l, elem).unwrap_or_else(|_| *elem.clone());
            Ok(DataType::Array(Box::new(result_elem)))
        }
        _ => Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!("cannot perform arithmetic on {left} and {right}"),
        )))),
    }
}

pub(super) fn resolve_set_operation_type(
    left: &DataType,
    right: &DataType,
    left_expr: Option<&TypedExpr>,
    right_expr: Option<&TypedExpr>,
    left_unknown_hint: bool,
    right_unknown_hint: bool,
) -> DbResult<DataType> {
    if left == right {
        return Ok(left.clone());
    }

    let left_unknown = left_unknown_hint || is_set_operation_unknown(left_expr);
    let right_unknown = right_unknown_hint || is_set_operation_unknown(right_expr);

    if left_unknown && !right_unknown {
        return Ok(right.clone());
    }
    if right_unknown && !left_unknown {
        return Ok(left.clone());
    }

    if is_set_operation_numeric(left, left_unknown)
        && is_set_operation_numeric(right, right_unknown)
    {
        return match (left_unknown, right_unknown) {
            (true, false) => Ok(right.clone()),
            (false, true) => Ok(left.clone()),
            _ => resolve_arithmetic_type(left, right),
        };
    }

    match (left, right) {
        (DataType::Text, DataType::Text) => Ok(DataType::Text),
        (DataType::Date, DataType::Timestamp) | (DataType::Timestamp, DataType::Date) => {
            Ok(DataType::Timestamp)
        }
        (DataType::Date, DataType::TimestampTz)
        | (DataType::TimestampTz, DataType::Date)
        | (DataType::Timestamp, DataType::TimestampTz)
        | (DataType::TimestampTz, DataType::Timestamp)
        | (DataType::Time, DataType::TimeTz)
        | (DataType::TimeTz, DataType::Time) => Ok(DataType::TimestampTz),
        (DataType::Array(left_elem), DataType::Array(right_elem)) => Ok(DataType::Array(Box::new(
            resolve_set_operation_type(left_elem, right_elem, None, None, false, false)?,
        ))),
        (
            DataType::Vector {
                dims: left_dims, ..
            },
            DataType::Vector {
                dims: right_dims, ..
            },
        ) if left_dims == right_dims => Ok(left.clone()),
        (DataType::Jsonb, DataType::Jsonb) => Ok(DataType::Jsonb),
        _ => Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "set operation types {} and {} cannot be matched",
                left.pg_type_name(),
                right.pg_type_name()
            ),
        )))),
    }
}

pub(super) fn validate_update_assignment(
    expr: &TypedExpr,
    column: &ColumnDescriptor,
    is_parameter: bool,
) -> DbResult<()> {
    validate_assignment_expr(
        expr,
        &column.data_type,
        column.nullable,
        is_parameter,
        "UPDATE",
    )
}

pub(super) fn validate_assignment_expr(
    expr: &TypedExpr,
    target_type: &DataType,
    _nullable: bool,
    is_parameter: bool,
    operation: &str,
) -> DbResult<()> {
    if is_assignment_compatible(&expr.data_type, target_type, &expr.kind, is_parameter) {
        Ok(())
    } else {
        Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!("{operation} expression cannot be coerced to {target_type:?} yet"),
        ))))
    }
}

/// Check whether an expression of `source` type can be assigned to a column of
/// `target` type.  This mirrors PostgreSQL's implicit assignment-cast rules:
/// any type can be cast to Text (via Display), Text can be cast to almost any
/// type (string-literal coercion), all numeric types are inter-assignable,
/// arrays are coercible when their element types are, and so on.  The runtime
/// `cast_value` function handles the actual conversion.
pub(super) fn is_assignment_compatible(
    source: &DataType,
    target: &DataType,
    kind: &TypedExprKind,
    is_parameter: bool,
) -> bool {
    // Null is always compatible.
    if matches!(kind, TypedExprKind::Literal(Value::Null)) {
        return true;
    }
    // Exact type match.
    if source == target {
        return true;
    }
    // Prepared-statement parameters adopt the target type.
    if is_parameter {
        return true;
    }
    // Text (string literal / unknown) can be coerced to any target type  -
    // PostgreSQL treats unadorned string literals as type "unknown" and
    // resolves them to whatever the target column requires.
    if matches!(source, DataType::Text) {
        return true;
    }
    // Any type can be displayed as Text.
    if matches!(target, DataType::Text) {
        return true;
    }
    // All numeric types are inter-assignable (may truncate at runtime).
    if is_numeric(source) && is_numeric(target) {
        return true;
    }
    if matches!(
        (source, target),
        (DataType::Money, DataType::Money)
            | (
                DataType::Money,
                DataType::Int
                    | DataType::BigInt
                    | DataType::Numeric
                    | DataType::Real
                    | DataType::Double
            )
            | (
                DataType::Int
                    | DataType::BigInt
                    | DataType::Numeric
                    | DataType::Real
                    | DataType::Double,
                DataType::Money
            )
    ) {
        return true;
    }
    // Boolean <-> numeric coercions (PG: TRUE=1, FALSE=0).
    if matches!(
        (source, target),
        (
            DataType::Boolean,
            DataType::Int
                | DataType::BigInt
                | DataType::Numeric
                | DataType::Real
                | DataType::Double
        ) | (
            DataType::Int
                | DataType::BigInt
                | DataType::Numeric
                | DataType::Real
                | DataType::Double,
            DataType::Boolean
        )
    ) {
        return true;
    }
    // Date/time family cross-casts.
    if matches!(
        (source, target),
        (DataType::Date, DataType::Timestamp | DataType::TimestampTz)
            | (
                DataType::Timestamp,
                DataType::Date | DataType::TimestampTz | DataType::Time | DataType::TimeTz
            )
            | (
                DataType::TimestampTz,
                DataType::Date | DataType::Timestamp | DataType::Time | DataType::TimeTz
            )
            | (DataType::Time, DataType::TimeTz)
            | (DataType::TimeTz, DataType::Time)
            | (DataType::TimeTz, DataType::Interval)
            | (DataType::Interval, DataType::TimeTz)
            | (DataType::Time, DataType::Interval)
            | (DataType::Interval, DataType::Time)
    ) {
        return true;
    }
    // Jsonb <-> other scalar types (the runtime handles extraction / parsing).
    if matches!(source, DataType::Jsonb) || matches!(target, DataType::Jsonb) {
        return true;
    }
    // Array coercion: Array(A) is assignable to Array(B) when A is
    // assignable to B (e.g. Array(BigInt) -> Array(Int)).
    if let (DataType::Array(src_elem), DataType::Array(tgt_elem)) = (source, target) {
        // Recurse on element types - use a dummy literal kind for the
        // inner check (we only care about the data-type relationship).
        return is_assignment_compatible(
            src_elem,
            tgt_elem,
            &TypedExprKind::Literal(Value::Null),
            false,
        );
    }
    // A scalar value may be assignable to an Array of that scalar
    // (PG allows single-element promotion in some contexts).
    if let DataType::Array(tgt_elem) = target {
        return is_assignment_compatible(
            source,
            tgt_elem,
            &TypedExprKind::Literal(Value::Null),
            false,
        );
    }
    // An Array expression being assigned to a scalar target - allow it
    // so that set-returning functions / array results can feed scalar
    // columns (the executor will extract or cast at runtime).
    if matches!(source, DataType::Array(_)) {
        return true;
    }
    // Blob <-> Text is handled above (Text arm), but Blob -> other types
    // should also be lenient.
    if matches!(source, DataType::Blob) || matches!(target, DataType::Blob) {
        return true;
    }
    // Vector types are compatible with each other regardless of dims.
    if matches!(
        (source, target),
        (DataType::Vector { .. }, DataType::Vector { .. })
    ) {
        return true;
    }
    false
}

/// Numeric family used for expression/set-operation coercions.
/// Includes `Money` for compatibility with arithmetic/coercion rules.
pub(super) fn is_numeric(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Int
            | DataType::BigInt
            | DataType::Real
            | DataType::Double
            | DataType::Numeric
            | DataType::Money
    )
}

/// Strict numeric family used in contexts that do not accept implicit `Money`
/// coercions (for example DML assignment checks and `width_bucket` typing).
pub(super) fn is_numeric_without_money(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Int | DataType::BigInt | DataType::Real | DataType::Double | DataType::Numeric
    )
}

fn is_set_operation_numeric(dt: &DataType, unknown_literal: bool) -> bool {
    is_numeric(dt) || (unknown_literal && matches!(dt, DataType::Text))
}

fn is_set_operation_unknown(expr: Option<&TypedExpr>) -> bool {
    matches!(
        expr.map(|typed| &typed.kind),
        Some(TypedExprKind::Literal(Value::Null | Value::Text(_)))
    )
}

fn is_orderable(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Int
            | DataType::BigInt
            | DataType::Real
            | DataType::Double
            | DataType::Numeric
            | DataType::Money
            | DataType::Text
            | DataType::Boolean
            | DataType::Blob
            | DataType::Timestamp
            | DataType::Date
            | DataType::Time
            | DataType::TimeTz
            | DataType::Interval
            | DataType::Tid
            | DataType::PgLsn
            | DataType::MacAddr
            | DataType::MacAddr8
            | DataType::Uuid
            | DataType::TimestampTz
            | DataType::Jsonb
            | DataType::Array(_)
    )
}

pub(super) fn ensure_comparable_for_eq(left: &TypedExpr, right: &TypedExpr) -> DbResult<()> {
    let comparable = left.data_type == right.data_type
        || (is_numeric(&left.data_type) && is_numeric(&right.data_type))
        || matches!(
            (&left.data_type, &right.data_type),
            (DataType::Timestamp, DataType::TimestampTz)
                | (DataType::TimestampTz, DataType::Timestamp)
                | (DataType::Date, DataType::Timestamp)
                | (DataType::Timestamp, DataType::Date)
                | (DataType::Date, DataType::TimestampTz)
                | (DataType::TimestampTz, DataType::Date)
                | (DataType::Time, DataType::TimeTz)
                | (DataType::TimeTz, DataType::Time)
        )
        // Text is comparable with any type (PG implicit coercion)
        || matches!(left.data_type, DataType::Text)
        || matches!(right.data_type, DataType::Text)
        // Boolean <-> numeric comparisons (PG: TRUE=1, FALSE=0)
        || (matches!(left.data_type, DataType::Boolean) && is_numeric(&right.data_type))
        || (is_numeric(&left.data_type) && matches!(right.data_type, DataType::Boolean))
        // Array types are comparable
        || matches!(
            (&left.data_type, &right.data_type),
            (DataType::Array(_), DataType::Array(_))
        )
        // Array with scalar (e.g. array contains element checks)
        || matches!(left.data_type, DataType::Array(_))
        || matches!(right.data_type, DataType::Array(_))
        // Jsonb with any type
        || matches!(left.data_type, DataType::Jsonb)
        || matches!(right.data_type, DataType::Jsonb);

    if comparable {
        Ok(())
    } else {
        Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "cannot compare values of type {:?} and {:?}",
                left.data_type, right.data_type
            ),
        ))))
    }
}

pub(super) fn ensure_orderable_comparison(left: &TypedExpr, right: &TypedExpr) -> DbResult<()> {
    let orderable = (left.data_type == right.data_type && is_orderable(&left.data_type))
        || (is_numeric(&left.data_type) && is_numeric(&right.data_type))
        || matches!(
            (&left.data_type, &right.data_type),
            (DataType::Timestamp, DataType::TimestampTz)
                | (DataType::TimestampTz, DataType::Timestamp)
                | (DataType::Date, DataType::Timestamp)
                | (DataType::Timestamp, DataType::Date)
                | (DataType::Date, DataType::TimestampTz)
                | (DataType::TimestampTz, DataType::Date)
                | (DataType::Time, DataType::TimeTz)
                | (DataType::TimeTz, DataType::Time)
        )
        // Text is order-comparable with any orderable type (PG implicit coercion)
        || (matches!(left.data_type, DataType::Text) && is_orderable(&right.data_type))
        || (is_orderable(&left.data_type) && matches!(right.data_type, DataType::Text))
        // Boolean <-> numeric ordering (PG: TRUE=1, FALSE=0)
        || (matches!(left.data_type, DataType::Boolean) && is_numeric(&right.data_type))
        || (is_numeric(&left.data_type) && matches!(right.data_type, DataType::Boolean));

    if orderable {
        Ok(())
    } else {
        Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "cannot order-compare values of type {:?} and {:?}",
                left.data_type, right.data_type
            ),
        ))))
    }
}

pub(super) fn ensure_orderable_sort_expr(expr: &TypedExpr) -> DbResult<()> {
    if matches!(
        expr.data_type,
        DataType::Int
            | DataType::BigInt
            | DataType::Real
            | DataType::Double
            | DataType::Numeric
            | DataType::Money
            | DataType::Text
            | DataType::Boolean
            | DataType::Blob
            | DataType::Timestamp
            | DataType::Date
            | DataType::Time
            | DataType::TimeTz
            | DataType::Interval
            | DataType::Tid
            | DataType::PgLsn
            | DataType::MacAddr
            | DataType::MacAddr8
            | DataType::Uuid
            | DataType::TimestampTz
            | DataType::Jsonb
            | DataType::Array(_)
    ) {
        Ok(())
    } else {
        Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!("cannot ORDER BY values of type {:?}", expr.data_type),
        ))))
    }
}

pub(super) fn contextualize_null(expr: TypedExpr, target_type: &DataType) -> TypedExpr {
    if matches!(expr.kind, TypedExprKind::Literal(Value::Null)) {
        TypedExpr::literal(Value::Null, target_type.clone(), true)
    } else {
        expr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_family_includes_money() {
        assert!(is_numeric(&DataType::Money));
        assert!(is_numeric(&DataType::Numeric));
    }

    #[test]
    fn strict_numeric_family_excludes_money() {
        assert!(!is_numeric_without_money(&DataType::Money));
        assert!(is_numeric_without_money(&DataType::Double));
        assert!(is_numeric_without_money(&DataType::Numeric));
    }
}
