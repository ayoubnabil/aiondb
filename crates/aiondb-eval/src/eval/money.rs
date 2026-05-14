use crate::eval::scalar_functions::value_convert::{f64_to_i64, i64_to_f64};
use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState};

pub(crate) const MONEY_SCALE: u32 = 2;

pub(crate) fn money_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "money out of range",
    ))
}

pub(crate) fn money_value_out_of_range(input: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        format!("value \"{input}\" is out of range for type money"),
    ))
}

fn invalid_money_input(input: &str) -> DbError {
    DbError::invalid_input_syntax("money", input)
}

pub(crate) fn money_to_numeric(cents: i64) -> NumericValue {
    NumericValue::new(i128::from(cents), MONEY_SCALE)
}

pub(crate) fn numeric_to_money(value: &NumericValue) -> DbResult<i64> {
    if value.is_special() {
        return Err(money_out_of_range());
    }
    let rounded = value.round(MONEY_SCALE);
    if rounded.is_big() {
        return Err(money_out_of_range());
    }
    i64::try_from(rounded.coefficient).map_err(|_| money_out_of_range())
}

pub(crate) fn float_to_money(value: f64) -> DbResult<i64> {
    if !value.is_finite() {
        return Err(money_out_of_range());
    }
    let scaled = value * 100.0;
    // i64::MAX as f64 rounds up to 9223372036854775808.0 (one above the true
    // i64::MAX), so `>` would incorrectly allow that exact value through.
    // Using `>=` ensures every out-of-range f64 is rejected.
    if !scaled.is_finite() || scaled < i64_to_f64(i64::MIN) || scaled >= i64_to_f64(i64::MAX) {
        return Err(money_out_of_range());
    }
    f64_to_i64(scaled.round()).map_err(|_| money_out_of_range())
}

pub(crate) fn money_mul_i64(cents: i64, factor: i64) -> DbResult<i64> {
    cents.checked_mul(factor).ok_or_else(money_out_of_range)
}

pub(crate) fn money_div_i64(cents: i64, divisor: i64) -> DbResult<i64> {
    if divisor == 0 {
        return Err(DbError::internal("division by zero"));
    }
    cents.checked_div(divisor).ok_or_else(money_out_of_range)
}

// PostgreSQL multiplies/divides cents directly by the float factor
// (cash_mul_flt8 does `rint(c * f)`). Going through dollars first
// (`cents / 100.0 * factor`) introduces an extra rounding step.
pub(crate) fn money_mul_f64(cents: i64, factor: f64) -> DbResult<i64> {
    let result = (i64_to_f64(cents) * factor).round();
    if !result.is_finite() || result < i64_to_f64(i64::MIN) || result >= i64_to_f64(i64::MAX) {
        return Err(money_out_of_range());
    }
    f64_to_i64(result).map_err(|_| money_out_of_range())
}

pub(crate) fn money_div_f64(cents: i64, divisor: f64) -> DbResult<i64> {
    if divisor == 0.0 {
        return Err(DbError::internal("division by zero"));
    }
    let result = (i64_to_f64(cents) / divisor).round();
    if !result.is_finite() || result < i64_to_f64(i64::MIN) || result >= i64_to_f64(i64::MAX) {
        return Err(money_out_of_range());
    }
    f64_to_i64(result).map_err(|_| money_out_of_range())
}

pub(crate) fn parse_money_text(input: &str) -> DbResult<i64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(invalid_money_input(input));
    }

    let (negative, body) =
        normalize_money_body(trimmed).ok_or_else(|| invalid_money_input(input))?;
    let (integer_part, fractional_part) =
        parse_money_digits(body).ok_or_else(|| invalid_money_input(input))?;

    let integer = integer_part
        .parse::<i128>()
        .map_err(|_| money_value_out_of_range(input))?;
    let mut cents = integer
        .checked_mul(100)
        .ok_or_else(|| money_value_out_of_range(input))?;

    let frac_bytes = fractional_part.as_bytes();
    let d1 = frac_bytes.first().map_or(0_i128, |b| i128::from(b - b'0'));
    let d2 = frac_bytes.get(1).map_or(0_i128, |b| i128::from(b - b'0'));
    cents = cents
        .checked_add(d1 * 10 + d2)
        .ok_or_else(|| money_value_out_of_range(input))?;
    if frac_bytes.get(2).is_some_and(|b| *b >= b'5') {
        cents = cents
            .checked_add(1)
            .ok_or_else(|| money_value_out_of_range(input))?;
    }

    let signed = if negative { -cents } else { cents };
    i64::try_from(signed).map_err(|_| money_value_out_of_range(input))
}

fn normalize_money_body(input: &str) -> Option<(bool, &str)> {
    let mut negative = false;
    let mut body = input.trim();

    if let Some(inner) = body
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    {
        negative = true;
        body = inner.trim();
    }

    loop {
        let next = if let Some(rest) = body.strip_prefix('-') {
            negative = !negative;
            Some(rest)
        } else if let Some(rest) = body.strip_prefix('+') {
            Some(rest)
        } else {
            body.strip_prefix('$')
        };
        let Some(rest) = next else {
            break;
        };
        body = rest.trim();
    }

    if body.is_empty() || body.contains(['$', '(', ')']) {
        return None;
    }
    Some((negative, body))
}

fn parse_money_digits(body: &str) -> Option<(String, String)> {
    let mut integer = String::new();
    let mut fraction = String::new();
    let mut seen_decimal = false;

    for ch in body.chars() {
        match ch {
            '0'..='9' => {
                if seen_decimal {
                    fraction.push(ch);
                } else {
                    integer.push(ch);
                }
            }
            ',' if !seen_decimal => {}
            '.' if !seen_decimal => seen_decimal = true,
            _ => return None,
        }
    }

    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    if integer.is_empty() {
        integer.push('0');
    }
    Some((integer, fraction))
}

pub(crate) fn money_to_words(cents: i64) -> String {
    let negative = cents < 0;
    let abs_cents = cents.unsigned_abs();
    let dollars = abs_cents / 100;
    let cents_part = abs_cents % 100;

    let mut rendered = String::new();
    if negative {
        rendered.push_str("Minus ");
    }
    rendered.push_str(&number_to_words(dollars));
    rendered.push(' ');
    rendered.push_str(if dollars == 1 { "dollar" } else { "dollars" });
    rendered.push_str(" and ");
    rendered.push_str(&number_to_words(cents_part));
    rendered.push(' ');
    rendered.push_str(if cents_part == 1 { "cent" } else { "cents" });
    capitalize_ascii_first(&mut rendered);
    rendered
}

fn number_to_words(value: u64) -> String {
    if value == 0 {
        return "zero".to_owned();
    }

    const GROUPS: [&str; 6] = [
        "",
        "thousand",
        "million",
        "billion",
        "trillion",
        "quadrillion",
    ];
    let mut n = value;
    let mut parts = Vec::new();
    let mut group_idx = 0;

    while n > 0 {
        let chunk = u16::try_from(n % 1000).unwrap_or(0);
        if chunk != 0 {
            let mut chunk_words = chunk_to_words(chunk);
            let suffix = GROUPS.get(group_idx).copied().unwrap_or("");
            if !suffix.is_empty() {
                chunk_words.push(' ');
                chunk_words.push_str(suffix);
            }
            parts.push(chunk_words);
        }
        n /= 1000;
        group_idx += 1;
    }

    parts.reverse();
    parts.join(" ")
}

fn capitalize_ascii_first(input: &mut str) {
    if let Some(first) = input.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
}

fn chunk_to_words(chunk: u16) -> String {
    const SMALL: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];

    let hundreds = chunk / 100;
    let rest = chunk % 100;
    let mut parts = Vec::new();

    if hundreds > 0 {
        parts.push(format!("{} hundred", SMALL[usize::from(hundreds)]));
    }

    if rest > 0 {
        if rest < 20 {
            parts.push(SMALL[usize::from(rest)].to_owned());
        } else {
            let tens = rest / 10;
            let units = rest % 10;
            if units == 0 {
                parts.push(TENS[usize::from(tens)].to_owned());
            } else {
                parts.push(format!(
                    "{} {}",
                    TENS[usize::from(tens)],
                    SMALL[usize::from(units)]
                ));
            }
        }
    }

    parts.join(" ")
}
