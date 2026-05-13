use unicode_normalization::UnicodeNormalization;

use aiondb_core::{DbError, DbResult, Value};

use super::value_convert::i64_to_f64;
use super::{cypher_temporal, ext, utility};

pub(crate) fn expect_args(args: &[Value], expected: usize, name: &str) -> DbResult<()> {
    if args.len() != expected {
        return Err(DbError::internal(format!(
            "{name} requires {expected} argument(s), got {}",
            args.len()
        )));
    }
    Ok(())
}

pub(crate) fn expect_at_least_args(args: &[Value], minimum: usize, name: &str) -> DbResult<()> {
    if args.len() < minimum {
        let plural = if minimum == 1 { "" } else { "s" };
        return Err(DbError::internal(format!(
            "{name} requires at least {minimum} argument{plural}"
        )));
    }
    Ok(())
}

pub(crate) fn expect_arg_range(
    args: &[Value],
    minimum: usize,
    maximum: usize,
    message: &str,
) -> DbResult<()> {
    if args.len() < minimum || args.len() > maximum {
        return Err(DbError::internal(message));
    }
    Ok(())
}

pub(crate) fn to_f64(val: &Value) -> DbResult<f64> {
    match val {
        Value::Int(v) => Ok(f64::from(*v)),
        Value::BigInt(v) => Ok(i64_to_f64(*v)),
        Value::Real(v) => Ok(f64::from(*v)),
        Value::Double(v) => Ok(*v),
        Value::Numeric(n) => Ok(crate::eval::cast::numeric::numeric_to_f64(n)),
        Value::Text(s) => {
            let trimmed = s.trim();
            if trimmed.eq_ignore_ascii_case("inf")
                || trimmed.eq_ignore_ascii_case("+inf")
                || trimmed.eq_ignore_ascii_case("infinity")
                || trimmed.eq_ignore_ascii_case("+infinity")
            {
                Ok(f64::INFINITY)
            } else if trimmed.eq_ignore_ascii_case("-inf")
                || trimmed.eq_ignore_ascii_case("-infinity")
            {
                Ok(f64::NEG_INFINITY)
            } else if trimmed.eq_ignore_ascii_case("nan") {
                Ok(f64::NAN)
            } else {
                trimmed.parse::<f64>().map_err(|_| {
                    DbError::internal(
                        "expected a numeric value (Int, BigInt, Real, Double, or Numeric)",
                    )
                })
            }
        }
        Value::Boolean(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(DbError::internal(
            "expected a numeric value (Int, BigInt, Real, Double, or Numeric)",
        )),
    }
}

pub(crate) fn expect_text_arg<'a>(
    args: &'a [Value],
    index: usize,
    name: &str,
    position: &str,
) -> DbResult<&'a str> {
    match args.get(index) {
        Some(Value::Text(text)) => Ok(text.as_str()),
        Some(_) => Err(DbError::internal(format!(
            "{name} {position} arg must be text"
        ))),
        None => Err(DbError::internal(format!("{name} missing {position} arg"))),
    }
}

pub(crate) fn value_to_text(v: &Value) -> String {
    utility::value_to_text(v)
}

pub fn eval_cypher_temporal_property_access(base: &Value, field: &str) -> Option<Value> {
    cypher_temporal::operations::temporal_property_access(base, field)
}

pub(crate) fn unsupported_named_function(name: &str) -> DbResult<Value> {
    Err(DbError::feature_not_supported(format!(
        "function \"{name}\" is recognized but not implemented"
    )))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnicodeNormalizationForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

fn parse_unicode_normalization_form(value: &Value) -> DbResult<UnicodeNormalizationForm> {
    // Borrow the Text payload via Cow when possible - `unicode_normalize`
    // arguments are short ASCII tokens (`'NFC'` / `'NFD'` / `'NFKC'` /
    // `'NFKD'`) so cloning the input or routing it through a value→text
    // dispatch is wasted work; a Cow lets us hand the canonical accept
    // arms a borrowed `&str` directly.
    let raw: std::borrow::Cow<'_, str> = match value {
        Value::Text(text) => std::borrow::Cow::Borrowed(text.as_str()),
        other => std::borrow::Cow::Owned(value_to_text(other)),
    };
    let raw = raw.as_ref();

    if raw.eq_ignore_ascii_case("NFC") {
        return Ok(UnicodeNormalizationForm::Nfc);
    }
    if raw.eq_ignore_ascii_case("NFD") {
        return Ok(UnicodeNormalizationForm::Nfd);
    }
    if raw.eq_ignore_ascii_case("NFKC") {
        return Ok(UnicodeNormalizationForm::Nfkc);
    }
    if raw.eq_ignore_ascii_case("NFKD") {
        return Ok(UnicodeNormalizationForm::Nfkd);
    }
    Err(DbError::syntax_error(format!(
        "invalid normalization form: {raw}"
    )))
}

fn normalize_unicode_text(input: &str, form: UnicodeNormalizationForm) -> String {
    match form {
        UnicodeNormalizationForm::Nfc => input.nfc().collect(),
        UnicodeNormalizationForm::Nfd => input.nfd().collect(),
        UnicodeNormalizationForm::Nfkc => input.nfkc().collect(),
        UnicodeNormalizationForm::Nfkd => input.nfkd().collect(),
    }
}

fn prepare_unicode_normalization_input(
    args: &[Value],
    function_name: &str,
) -> DbResult<Option<(String, UnicodeNormalizationForm)>> {
    if !(1..=2).contains(&args.len()) {
        return Err(DbError::internal(format!(
            "{function_name} requires 1 or 2 arguments, got {}",
            args.len()
        )));
    }
    let input = &args[0];
    if input.is_null() || matches!(args.get(1), Some(Value::Null)) {
        return Ok(None);
    }

    let form = match args.get(1) {
        Some(value) => parse_unicode_normalization_form(value)?,
        None => UnicodeNormalizationForm::Nfc,
    };
    let input = match input {
        Value::Text(text) => text.clone(),
        other => value_to_text(other),
    };
    Ok(Some((input, form)))
}

pub(crate) fn eval_normalize(args: &[Value]) -> DbResult<Value> {
    let Some((input, form)) = prepare_unicode_normalization_input(args, "normalize")? else {
        return Ok(Value::Null);
    };
    Ok(Value::Text(normalize_unicode_text(&input, form)))
}

pub(crate) fn eval_is_normalized(args: &[Value]) -> DbResult<Value> {
    let Some((input, form)) = prepare_unicode_normalization_input(args, "is_normalized")? else {
        return Ok(Value::Null);
    };
    Ok(Value::Boolean(
        normalize_unicode_text(&input, form) == input,
    ))
}

pub(crate) fn eval_pg_boolean_comparison(name: &str, args: &[Value]) -> Option<DbResult<Value>> {
    let result = match name {
        "booleq" | "boolne" | "boollt" | "boolgt" | "boolle" | "boolge" => {
            if let Err(err) = expect_args(args, 2, name) {
                return Some(Err(err));
            }
            if args.iter().any(Value::is_null) {
                return Some(Ok(Value::Null));
            }

            let (left, right) = match (&args[0], &args[1]) {
                (Value::Boolean(left), Value::Boolean(right)) => (*left, *right),
                _ => {
                    return Some(Err(DbError::internal(format!(
                        "{name}() arguments must be boolean"
                    ))));
                }
            };

            let ordering = left.cmp(&right);
            Value::Boolean(match name {
                "booleq" => ordering.is_eq(),
                "boolne" => !ordering.is_eq(),
                "boollt" => ordering.is_lt(),
                "boolgt" => ordering.is_gt(),
                "boolle" => !ordering.is_gt(),
                "boolge" => !ordering.is_lt(),
                _ => unreachable!(),
            })
        }
        _ => return None,
    };

    Some(Ok(result))
}

#[inline]
pub(crate) fn eval_quantified_array_generic(name: &str, args: &[Value]) -> Option<DbResult<Value>> {
    let op_name = name
        .strip_prefix("__aiondb_quantified_any_")
        .or_else(|| name.strip_prefix("__aiondb_quantified_all_"))?;

    if matches!(op_name, "like" | "not_like" | "ilike" | "not_ilike") {
        Some(ext::eval_quantified_like(name, args))
    } else if matches!(
        op_name,
        "regex_match" | "regex_match_ci" | "not_regex_match" | "not_regex_match_ci"
    ) {
        Some(ext::eval_quantified_regex(name, args))
    } else {
        Some(ext::eval_quantified_comparison(name, args))
    }
}
