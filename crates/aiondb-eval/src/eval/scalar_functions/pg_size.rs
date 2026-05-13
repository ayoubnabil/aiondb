use super::*;
use aiondb_core::{
    compat_server_version_num_string, compat_setting_value, COMPAT_CLIENT_ENCODING,
    COMPAT_DEFAULT_DATABASE_NAME,
};

pub(super) fn current_setting_default(name: &str) -> String {
    // Names from `current_setting('…')` callers are conventionally
    // already lowercase. Match via `eq_ignore_ascii_case` to avoid
    // the per-call `to_ascii_lowercase()` String alloc.
    if name.eq_ignore_ascii_case("is_superuser") {
        return "on".to_string();
    }
    if name.eq_ignore_ascii_case("max_connections") {
        return "128".to_string();
    }
    if name.eq_ignore_ascii_case("server_version_num") {
        return compat_server_version_num_string();
    }
    if name.eq_ignore_ascii_case("current_catalog") {
        return COMPAT_DEFAULT_DATABASE_NAME.to_owned();
    }
    if name.eq_ignore_ascii_case("server_encoding") || name.eq_ignore_ascii_case("client_encoding")
    {
        return COMPAT_CLIENT_ENCODING.to_owned();
    }
    compat_setting_value(name)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default()
}

pub(super) fn eval_pg_size_pretty(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_size_pretty")?;
    let size = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Int(v) => NumericValue::from_i32(*v),
        Value::BigInt(v) => NumericValue::from_i64(*v),
        Value::Numeric(v) => v.clone(),
        other => {
            return Err(DbError::syntax_error(format!(
                "pg_size_pretty does not support {}",
                other.data_type().unwrap_or(DataType::Text)
            )));
        }
    };

    let negative = size.coefficient < 0;
    let abs = size.abs();
    let units = ["bytes", "kB", "MB", "GB", "TB", "PB"];

    let mut unit_idx = 0usize;
    while unit_idx + 1 < units.len() {
        let unit_exp =
            u32::try_from(unit_idx).map_err(|_| DbError::syntax_error("bigint out of range"))?;
        let threshold_exp = unit_exp
            .checked_add(1)
            .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
        let threshold = 10_i128
            .checked_mul(
                checked_pow_i128(1024, threshold_exp)
                    .ok_or_else(|| DbError::syntax_error("bigint out of range"))?,
            )
            .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
        let scaled = scale_numeric_to_integer(&abs, 0)?;
        if scaled < threshold {
            break;
        }
        unit_idx += 1;
    }

    let body = if unit_idx == 0 && scale_numeric_to_integer(&abs, 0)? < 10_240 {
        format_numeric_compact(&abs)
    } else {
        let unit_exp =
            u32::try_from(unit_idx).map_err(|_| DbError::syntax_error("bigint out of range"))?;
        let mut divisor = checked_pow_i128(1024, unit_exp)
            .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
        let mut rounded = round_div_numeric_to_i128(&abs, divisor)?;
        if rounded == 10_240 && unit_idx + 1 < units.len() {
            unit_idx += 1;
            let next_unit_exp = u32::try_from(unit_idx)
                .map_err(|_| DbError::syntax_error("bigint out of range"))?;
            divisor = checked_pow_i128(1024, next_unit_exp)
                .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
            rounded = round_div_numeric_to_i128(&abs, divisor)?;
        }
        rounded.to_string()
    };

    Ok(Value::Text(format!(
        "{}{body} {}",
        if negative { "-" } else { "" },
        units[unit_idx]
    )))
}

pub(super) fn eval_pg_size_bytes(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_size_bytes")?;
    // Borrow the input on the Text fast path; other Value variants
    // still pay one Display alloc.
    let raw: std::borrow::Cow<'_, str> = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
        other => std::borrow::Cow::Owned(other.to_string()),
    };
    let input = raw.trim();
    if input.is_empty() {
        return Err(DbError::syntax_error(format!("invalid size: \"{raw}\"")));
    }

    let split = numeric_prefix_len(input);
    let number_part = &input[..split];
    let unit_part = input[split..].trim();
    if !valid_size_number(number_part) {
        return Err(DbError::syntax_error(format!("invalid size: \"{raw}\"")));
    }

    let number = match parse_scientific_numeric(number_part) {
        Ok(number) => number,
        Err(SizeNumberParseError::Invalid) => {
            return Err(DbError::syntax_error(format!("invalid size: \"{raw}\"")));
        }
        Err(SizeNumberParseError::BigIntOutOfRange) => {
            return Err(DbError::syntax_error("bigint out of range"));
        }
        Err(SizeNumberParseError::NumericOverflow) => {
            return Err(DbError::syntax_error("value overflows numeric format"));
        }
    };

    // Avoid `to_ascii_lowercase()` String alloc - match the
    // 7-token unit set via `eq_ignore_ascii_case`. SQL `pg_size_bytes`
    // accepts mixed-case input ("MB", "Mb", "mB").
    let factor = if unit_part.is_empty()
        || unit_part.eq_ignore_ascii_case("b")
        || unit_part.eq_ignore_ascii_case("bytes")
    {
        1_i128
    } else if unit_part.eq_ignore_ascii_case("kb") {
        1024_i128
    } else if unit_part.eq_ignore_ascii_case("mb") {
        1024_i128.pow(2)
    } else if unit_part.eq_ignore_ascii_case("gb") {
        1024_i128.pow(3)
    } else if unit_part.eq_ignore_ascii_case("tb") {
        1024_i128.pow(4)
    } else if unit_part.eq_ignore_ascii_case("pb") {
        1024_i128.pow(5)
    } else {
        return Err(invalid_size_unit_error(&raw, unit_part));
    };

    let coefficient = number
        .coefficient
        .checked_mul(factor)
        .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
    let divisor = checked_pow_i128(10, number.scale)
        .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))?;
    let upper_bound = (i64::MAX as i128)
        .checked_mul(divisor)
        .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
    let lower_bound = (i64::MIN as i128)
        .checked_mul(divisor)
        .ok_or_else(|| DbError::syntax_error("bigint out of range"))?;
    if coefficient > upper_bound || coefficient < lower_bound {
        return Err(DbError::syntax_error("bigint out of range"));
    }
    let truncated = coefficient / divisor;
    let bytes =
        i64::try_from(truncated).map_err(|_| DbError::syntax_error("bigint out of range"))?;
    Ok(Value::BigInt(bytes))
}

fn invalid_size_unit_error(raw: &str, unit: &str) -> DbError {
    DbError::syntax_error(format!("invalid size: \"{raw}\""))
        .with_client_detail(format!("Invalid size unit: \"{}\".", unit.trim()))
        .with_client_hint(
            "Valid units are \"bytes\", \"B\", \"kB\", \"MB\", \"GB\", \"TB\", and \"PB\".",
        )
}

fn scale_numeric_to_integer(value: &NumericValue, target_scale: u32) -> DbResult<i128> {
    if value.scale <= target_scale {
        let mul = checked_pow_i128(10, target_scale - value.scale)
            .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))?;
        value
            .coefficient
            .checked_mul(mul)
            .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))
    } else {
        let div = checked_pow_i128(10, value.scale - target_scale)
            .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))?;
        Ok(value.coefficient / div)
    }
}

fn round_div_numeric_to_i128(value: &NumericValue, divisor: i128) -> DbResult<i128> {
    let scale = checked_pow_i128(10, value.scale)
        .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))?;
    let den = scale
        .checked_mul(divisor)
        .ok_or_else(|| DbError::syntax_error("value overflows numeric format"))?;
    Ok((value.coefficient.abs() + den / 2) / den)
}

fn format_numeric_compact(value: &NumericValue) -> String {
    let mut s = value.to_string();
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

fn checked_pow_i128(base: i128, exp: u32) -> Option<i128> {
    let mut out = 1_i128;
    for _ in 0..exp {
        out = out.checked_mul(base)?;
    }
    Some(out)
}

fn numeric_prefix_len(input: &str) -> usize {
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_exp = false;
    let mut last_was_exp = false;
    let mut end = 0;

    for (idx, ch) in input.char_indices() {
        let accept = if idx == 0 {
            ch == '+' || ch == '-' || ch.is_ascii_digit() || ch == '.'
        } else if ch.is_ascii_digit() {
            seen_digit = true;
            last_was_exp = false;
            true
        } else if ch == '.' && !seen_dot && !seen_exp {
            seen_dot = true;
            last_was_exp = false;
            true
        } else if (ch == 'e' || ch == 'E') && seen_digit && !seen_exp {
            seen_exp = true;
            last_was_exp = true;
            true
        } else if (ch == '+' || ch == '-') && last_was_exp {
            last_was_exp = false;
            true
        } else {
            false
        };
        if !accept {
            break;
        }
        if ch.is_ascii_digit() {
            seen_digit = true;
        }
        end = idx + ch.len_utf8();
    }

    end
}

fn valid_size_number(number: &str) -> bool {
    if number.is_empty() {
        return false;
    }
    let has_digit = number.chars().any(|ch| ch.is_ascii_digit());
    if !has_digit {
        return false;
    }
    !number.ends_with('e')
        && !number.ends_with('E')
        && !number.ends_with('+')
        && !number.ends_with('-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeNumberParseError {
    Invalid,
    BigIntOutOfRange,
    NumericOverflow,
}

fn parse_scientific_numeric(input: &str) -> Result<NumericValue, SizeNumberParseError> {
    let (base, exp) = if let Some(idx) = input.find(['e', 'E']) {
        let exponent = input[idx + 1..]
            .parse::<i32>()
            .map_err(|_| SizeNumberParseError::NumericOverflow)?;
        (&input[..idx], exponent)
    } else {
        (input, 0)
    };

    let mut numeric: NumericValue = base.parse().map_err(|_| SizeNumberParseError::Invalid)?;
    if exp >= 0 {
        let exp = u32::try_from(exp).map_err(|_| SizeNumberParseError::NumericOverflow)?;
        if exp >= numeric.scale {
            let shift = exp - numeric.scale;
            let mul = checked_pow_i128(10, shift).ok_or(SizeNumberParseError::BigIntOutOfRange)?;
            numeric.coefficient = numeric
                .coefficient
                .checked_mul(mul)
                .ok_or(SizeNumberParseError::BigIntOutOfRange)?;
            numeric.scale = 0;
        } else {
            numeric.scale -= exp;
        }
    } else {
        numeric.scale = numeric
            .scale
            .checked_add(exp.unsigned_abs())
            .ok_or(SizeNumberParseError::NumericOverflow)?;
    }
    Ok(numeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_size_pretty_promotes_rounding_boundaries_to_next_unit() {
        let bigint_value =
            eval_pg_size_pretty(&[Value::BigInt(10_485_248)]).expect("boundary pretty");
        assert_eq!(bigint_value, Value::Text("10 MB".to_owned()));

        let numeric_value = eval_pg_size_pretty(&[Value::Numeric(
            "11258449312612352".parse().expect("numeric literal"),
        )])
        .expect("numeric boundary pretty");
        assert_eq!(numeric_value, Value::Text("10 PB".to_owned()));
    }

    #[test]
    fn pg_size_bytes_reports_invalid_unit_detail_and_hint() {
        let error = eval_pg_size_bytes(&[Value::Text("1 AB".to_owned())]).expect_err("invalid");
        let report = error.report();

        assert_eq!(report.message, "invalid size: \"1 AB\"");
        assert_eq!(
            report.client_detail.as_deref(),
            Some("Invalid size unit: \"AB\".")
        );
        assert_eq!(
            report.client_hint.as_deref(),
            Some("Valid units are \"bytes\", \"B\", \"kB\", \"MB\", \"GB\", \"TB\", and \"PB\".")
        );
    }

    #[test]
    fn pg_size_bytes_rejects_fractional_values_past_bigint_range() {
        let error = eval_pg_size_bytes(&[Value::Text("9223372036854775807.9".to_owned())])
            .expect_err("overflow should error");
        assert_eq!(error.report().message, "bigint out of range");
    }

    #[test]
    fn pg_size_bytes_keeps_truncation_for_fractional_inputs_in_range() {
        let value =
            eval_pg_size_bytes(&[Value::Text("-.1kb".to_owned())]).expect("fractional size");
        assert_eq!(value, Value::BigInt(-102));
    }

    #[test]
    fn pg_size_bytes_scientific_notation_reports_bigint_range_errors() {
        let error =
            eval_pg_size_bytes(&[Value::Text("1e100".to_owned())]).expect_err("should overflow");
        assert_eq!(error.report().message, "bigint out of range");
    }

    #[test]
    fn pg_size_bytes_scientific_notation_reports_numeric_overflow_errors() {
        let error = eval_pg_size_bytes(&[Value::Text("1e1000000000000000000".to_owned())])
            .expect_err("should exceed numeric format");
        assert_eq!(error.report().message, "value overflows numeric format");
    }
}
