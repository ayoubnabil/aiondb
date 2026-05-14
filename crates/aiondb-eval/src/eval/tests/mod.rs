use std::cmp::Ordering;

use super::*;
use aiondb_core::{DataType, IntervalValue, NumericValue, Row, Value};
use aiondb_plan::TypedExpr;
use time::{Date, Month, PrimitiveDateTime, Time};

mod arithmetic;
mod comparison_helpers;
mod comparisons;
mod datetime_functions;
mod depth_limit;
mod functions;
mod logic;
mod row;
mod text_functions;

// =====================================================================
// Helper functions
// =====================================================================

fn lit_int(v: i32) -> TypedExpr {
    TypedExpr::literal(Value::Int(v), DataType::Int, false)
}

fn lit_bigint(v: i64) -> TypedExpr {
    TypedExpr::literal(Value::BigInt(v), DataType::BigInt, false)
}

fn lit_real(v: f32) -> TypedExpr {
    TypedExpr::literal(Value::Real(v), DataType::Real, false)
}

fn lit_double(v: f64) -> TypedExpr {
    TypedExpr::literal(Value::Double(v), DataType::Double, false)
}

fn lit_text(s: &str) -> TypedExpr {
    TypedExpr::literal(Value::Text(s.to_string()), DataType::Text, false)
}

fn lit_bool(b: bool) -> TypedExpr {
    TypedExpr::literal(Value::Boolean(b), DataType::Boolean, false)
}

fn lit_null() -> TypedExpr {
    TypedExpr::literal(Value::Null, DataType::Boolean, true)
}

fn lit_numeric(coeff: i128, scale: u32) -> TypedExpr {
    TypedExpr::literal(
        Value::Numeric(NumericValue::new(coeff, scale)),
        DataType::Numeric,
        false,
    )
}

fn lit_blob(bytes: Vec<u8>) -> TypedExpr {
    TypedExpr::literal(Value::Blob(bytes), DataType::Blob, false)
}

fn lit_timestamp(year: i32, month: Month, day: u8, h: u8, m: u8, s: u8) -> TypedExpr {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(year, month, day).unwrap(),
        Time::from_hms(h, m, s).unwrap(),
    );
    TypedExpr::literal(Value::Timestamp(dt), DataType::Timestamp, false)
}

fn lit_date(year: i32, month: Month, day: u8) -> TypedExpr {
    let d = Date::from_calendar_date(year, month, day).unwrap();
    TypedExpr::literal(Value::Date(d), DataType::Date, false)
}

fn lit_interval(months: i32, days: i32, micros: i64) -> TypedExpr {
    TypedExpr::literal(
        Value::Interval(IntervalValue::new(months, days, micros)),
        DataType::Interval,
        false,
    )
}

fn eval(expr: &TypedExpr) -> DbResult<Value> {
    ExpressionEvaluator.evaluate(expr)
}

fn eval_row(expr: &TypedExpr, row: &Row) -> DbResult<Value> {
    ExpressionEvaluator.evaluate_with_row(expr, row)
}
