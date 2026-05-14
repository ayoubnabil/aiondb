use aiondb_core::{DataType, DbError, DbResult, Value};

use crate::eval::{cast::cast_value, money::money_to_words};

use super::expect_args;

fn value_to_money(value: &Value, function_name: &str) -> DbResult<i64> {
    match cast_value(value.clone(), &DataType::Money)? {
        Value::Money(cents) => Ok(cents),
        other => Err(DbError::internal(format!(
            "{function_name} expected money-compatible input, got {other}"
        ))),
    }
}

fn eval_cash_cmp(args: &[Value], name: &str, pick: fn(i64, i64) -> i64) -> DbResult<Value> {
    expect_args(args, 2, name)?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    Ok(Value::Money(pick(
        value_to_money(&args[0], name)?,
        value_to_money(&args[1], name)?,
    )))
}

pub(super) fn eval_cashlarger(args: &[Value]) -> DbResult<Value> {
    eval_cash_cmp(args, "cashlarger", i64::max)
}

pub(super) fn eval_cashsmaller(args: &[Value]) -> DbResult<Value> {
    eval_cash_cmp(args, "cashsmaller", i64::min)
}

pub(super) fn eval_cash_words(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "cash_words")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(Value::Text(money_to_words(value_to_money(
            value,
            "cash_words",
        )?))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_cashlarger_returns_null_when_any_arg_is_null() {
        let args = [Value::Money(125), Value::Null];
        let value = eval_cashlarger(&args).expect("cashlarger should handle nulls");
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn eval_cashsmaller_converts_non_money_inputs() {
        let args = [Value::Int(2), Value::Money(350)];
        let value = eval_cashsmaller(&args).expect("cashsmaller should cast inputs to money");
        assert_eq!(value, Value::Money(200));
    }

    #[test]
    fn eval_cash_words_returns_null_for_null_input() {
        let args = [Value::Null];
        let value = eval_cash_words(&args).expect("cash_words should handle null input");
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn eval_cash_words_converts_non_money_inputs() {
        let args = [Value::Int(2)];
        let value = eval_cash_words(&args).expect("cash_words should cast input to money");
        assert_eq!(value, Value::Text(money_to_words(200)));
    }
}
