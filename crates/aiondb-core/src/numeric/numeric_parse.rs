use std::str::FromStr;

use super::{
    exceeds_big_decimal_digits, BigCoefficient, NumericValue, MAX_BIG_DECIMAL_DIGITS,
    MAX_BIG_LIMBS, MAX_NUMERIC_LITERAL_LEN,
};

/// Validate underscore placement in numeric literals per `PostgreSQL` 16 rules.
fn validate_numeric_underscores(s: &str) -> Result<(), String> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let start = usize::from(!bytes.is_empty() && (bytes[0] == b'+' || bytes[0] == b'-'));
    let has_radix_prefix;
    let digit_start = if start + 2 <= len {
        let p0 = bytes[start];
        let p1 = bytes[start + 1];
        if p0 == b'0'
            && (p1 == b'x' || p1 == b'X' || p1 == b'o' || p1 == b'O' || p1 == b'b' || p1 == b'B')
        {
            has_radix_prefix = true;
            start + 2
        } else {
            has_radix_prefix = false;
            start
        }
    } else {
        has_radix_prefix = false;
        start
    };

    let is_hex = has_radix_prefix
        && start + 1 < len
        && (bytes[start + 1] == b'x' || bytes[start + 1] == b'X');

    for i in 0..len {
        if bytes[i] != b'_' {
            continue;
        }
        if i == len - 1 {
            return Err("invalid underscore placement".to_string());
        }
        if i < digit_start || (i == digit_start && !has_radix_prefix) {
            return Err("invalid underscore placement".to_string());
        }
        let prev = bytes[i - 1];
        let next = bytes[i + 1];
        if prev == b'_' || next == b'_' {
            return Err("invalid underscore placement".to_string());
        }
        if prev == b'.' || next == b'.' {
            return Err("invalid underscore placement".to_string());
        }
        if !is_hex && (prev == b'e' || prev == b'E' || next == b'e' || next == b'E') {
            return Err("invalid underscore placement".to_string());
        }
        if (prev == b'+' || prev == b'-') && (i == start + 1 || i == digit_start) {
            return Err("invalid underscore placement".to_string());
        }
    }
    Ok(())
}

#[must_use]
pub fn checked_ten_pow(n: u32) -> Option<i128> {
    const POWERS: [i128; 39] = [
        1,
        10,
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
        10_000_000_000,
        100_000_000_000,
        1_000_000_000_000,
        10_000_000_000_000,
        100_000_000_000_000,
        1_000_000_000_000_000,
        10_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000_000_000_000_000_000,
        100_000_000_000_000_000_000_000_000_000_000_000_000,
    ];
    POWERS.get(n as usize).copied()
}

impl FromStr for NumericValue {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty string".to_string());
        }
        if s.len() > MAX_NUMERIC_LITERAL_LEN {
            return Err("numeric value out of range".to_string());
        }
        // Skip the unconditional `s.to_ascii_lowercase()` allocation that
        // every NUMERIC text-cast paid just to recognise the small set of
        // special-value tokens. `eq_ignore_ascii_case` reads bytes
        // pairwise and short-circuits as soon as lengths differ - so a
        // typical decimal literal like `"3.14"` rejects every arm in O(1)
        // before reaching the digit-parsing fall-through.
        if s.eq_ignore_ascii_case("nan") {
            return Ok(Self::NAN);
        }
        if s.eq_ignore_ascii_case("inf")
            || s.eq_ignore_ascii_case("+inf")
            || s.eq_ignore_ascii_case("infinity")
            || s.eq_ignore_ascii_case("+infinity")
        {
            return Ok(Self::INFINITY);
        }
        if s.eq_ignore_ascii_case("-inf") || s.eq_ignore_ascii_case("-infinity") {
            return Ok(Self::NEG_INFINITY);
        }
        // Only pay the underscore-stripping String allocation when the
        // input actually contains an underscore. The dominant numeric
        // shape (digits, optional sign, optional decimal point, optional
        // exponent) never contains '_', so we can keep the original
        // `&str` and skip the allocation entirely on the hot path.
        let cleaned_storage: String;
        let s = if s.contains('_') {
            validate_numeric_underscores(s)?;
            cleaned_storage = s.chars().filter(|c| *c != '_').collect();
            cleaned_storage.as_str()
        } else {
            s
        };
        let (sign_negative, after_sign) = if let Some(rest) = s.strip_prefix('-') {
            (true, rest)
        } else if let Some(rest) = s.strip_prefix('+') {
            (false, rest)
        } else {
            (false, s)
        };
        if let Some(prefix_bytes) = after_sign.as_bytes().get(..2) {
            // Compare against ASCII radix prefixes by bytes so that an
            // adversarial multi-byte UTF-8 leading char cannot trigger a
            // string-slice panic at a non-char-boundary index. The suffix
            // slice is only formed once a match guarantees both prefix
            // bytes are ASCII (and thus byte 2 is at a char boundary).
            let radix = if prefix_bytes.eq_ignore_ascii_case(b"0x") {
                Some(16u32)
            } else if prefix_bytes.eq_ignore_ascii_case(b"0o") {
                Some(8)
            } else if prefix_bytes.eq_ignore_ascii_case(b"0b") {
                Some(2)
            } else {
                None
            };
            if let Some(radix) = radix {
                return parse_numeric_radix(&after_sign[2..], radix, sign_negative);
            }
        }
        if let Some(e_pos) = s.find(['e', 'E']) {
            return parse_numeric_scientific(s, e_pos);
        }
        match s.find('.') {
            Some(dot_pos) => {
                let int_part = &s[..dot_pos];
                let frac_part = &s[dot_pos + 1..];
                let scale = u32::try_from(frac_part.len()).unwrap_or(u32::MAX);
                if let Some(coefficient) = parse_decimal_i128_parts(int_part, frac_part) {
                    Ok(Self::new(coefficient, scale))
                } else {
                    let mut combined = String::with_capacity(int_part.len() + frac_part.len());
                    combined.push_str(int_part);
                    combined.push_str(frac_part);
                    parse_big_numeric(&combined, scale, s)
                }
            }
            None => match s.parse::<i128>() {
                Ok(coefficient) => Ok(Self::new(coefficient, 0)),
                Err(_) => parse_big_numeric(s, 0, s),
            },
        }
    }
}

fn parse_decimal_i128_parts(int_part: &str, frac_part: &str) -> Option<i128> {
    let (negative, int_digits) = if let Some(rest) = int_part.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = int_part.strip_prefix('+') {
        (false, rest)
    } else {
        (false, int_part)
    };

    if !int_digits.bytes().all(|byte| byte.is_ascii_digit())
        || !frac_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut coefficient = 0i128;
    let mut saw_digit = false;
    for digit in int_digits.bytes().chain(frac_part.bytes()) {
        saw_digit = true;
        let digit_value = i128::from(digit - b'0');
        coefficient = if negative {
            coefficient.checked_mul(10)?.checked_sub(digit_value)?
        } else {
            coefficient.checked_mul(10)?.checked_add(digit_value)?
        };
    }

    saw_digit.then_some(coefficient)
}

/// Parse a numeric string that overflows i128 into a big-coefficient representation.
fn parse_big_numeric(
    coefficient_str: &str,
    scale: u32,
    original: &str,
) -> Result<NumericValue, String> {
    let negative = coefficient_str.starts_with('-');
    let digits = if negative {
        &coefficient_str[1..]
    } else {
        coefficient_str
    };
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid numeric: {original}"));
    }
    if exceeds_big_decimal_digits(digits) {
        return Err("numeric value out of range".to_string());
    }
    let big = BigCoefficient::from_decimal_string(digits, negative);
    if big.limbs.len() > MAX_BIG_LIMBS {
        return Err("numeric value out of range".to_string());
    }
    Ok(NumericValue::from_big(big, scale))
}

/// Parse a numeric value with scientific notation.
fn parse_numeric_scientific(s: &str, e_pos: usize) -> Result<NumericValue, String> {
    let mantissa = &s[..e_pos];
    let exponent: i32 = s[e_pos + 1..]
        .parse()
        .map_err(|e| format!("invalid numeric exponent: {e}"))?;

    let (coefficient_str, scale) = if let Some(dot_pos) = mantissa.find('.') {
        let int_part = &mantissa[..dot_pos];
        let frac_part = &mantissa[dot_pos + 1..];
        (
            format!("{int_part}{frac_part}"),
            i32::try_from(frac_part.len()).unwrap_or(i32::MAX),
        )
    } else {
        (mantissa.to_owned(), 0i32)
    };

    let mantissa_digits = coefficient_str
        .chars()
        .filter(char::is_ascii_digit)
        .count()
        .try_into()
        .unwrap_or(i64::MAX);
    let max_digits = i64::try_from(MAX_BIG_DECIMAL_DIGITS).unwrap_or(i64::MAX);
    if exponent >= 0 && mantissa_digits.saturating_add(i64::from(exponent)) > max_digits {
        return Err("numeric exponent out of range".to_owned());
    }

    let new_scale = i64::from(scale) - i64::from(exponent);
    if new_scale > max_digits {
        return Err("numeric exponent out of range".to_owned());
    }
    if new_scale < 0 {
        let factor_exp =
            u32::try_from(-new_scale).map_err(|_| "numeric exponent out of range".to_owned())?;
        if let Ok(coeff) = coefficient_str.parse::<i128>() {
            if let Some(factor) = checked_ten_pow(factor_exp) {
                if let Some(result) = coeff.checked_mul(factor) {
                    return Ok(NumericValue::new(result, 0));
                }
            }
            let big = BigCoefficient::from_i128(coeff);
            if let Some(scaled) = big.mul_pow10(factor_exp) {
                return Ok(NumericValue::from_big(scaled, 0));
            }
        } else {
            let negative = coefficient_str.starts_with('-');
            let digits = if negative {
                &coefficient_str[1..]
            } else {
                &coefficient_str[..]
            };
            if exceeds_big_decimal_digits(digits) {
                return Err("numeric exponent out of range".to_owned());
            }
            let big = BigCoefficient::from_decimal_string(digits, negative);
            if let Some(scaled) = big.mul_pow10(factor_exp) {
                return Ok(NumericValue::from_big(scaled, 0));
            }
        }
        Err("numeric exponent out of range".to_owned())
    } else {
        let final_scale =
            u32::try_from(new_scale).map_err(|_| "numeric exponent out of range".to_owned())?;
        if let Ok(coeff) = coefficient_str.parse::<i128>() {
            Ok(NumericValue::new(coeff, final_scale))
        } else {
            let negative = coefficient_str.starts_with('-');
            let digits = if negative {
                &coefficient_str[1..]
            } else {
                &coefficient_str[..]
            };
            if exceeds_big_decimal_digits(digits) {
                return Err("numeric exponent out of range".to_owned());
            }
            let big = BigCoefficient::from_decimal_string(digits, negative);
            Ok(NumericValue::from_big(big, final_scale))
        }
    }
}

/// Parse a numeric value from hex, octal, or binary string.
fn parse_numeric_radix(digits: &str, radix: u32, negative: bool) -> Result<NumericValue, String> {
    let value = i128::from_str_radix(digits, radix).map_err(|e| format!("invalid numeric: {e}"))?;
    Ok(NumericValue::new(if negative { -value } else { value }, 0))
}
