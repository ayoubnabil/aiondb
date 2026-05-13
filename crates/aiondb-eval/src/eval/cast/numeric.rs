use crate::eval::scalar_functions::value_convert::{i128_to_f64, to_i32_saturating};
use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState, Value};

const NUMERIC_MAX_DISPLAY_SCALE: u32 = 1_000;

fn numeric_special_to_int_error(v: &NumericValue, type_name: &str) -> DbError {
    if v.is_nan() {
        DbError::from_report(ErrorReport::new(
            SqlState::FeatureNotSupported,
            format!("cannot convert NaN to {type_name}"),
        ))
    } else {
        DbError::from_report(ErrorReport::new(
            SqlState::FeatureNotSupported,
            format!("cannot convert infinity to {type_name}"),
        ))
    }
}

pub(crate) fn numeric_to_i16(v: &NumericValue) -> DbResult<Value> {
    if v.is_special() {
        return Err(numeric_special_to_int_error(v, "smallint"));
    }
    let rounded = v.round(0);
    if rounded.is_big() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "smallint out of range",
        )));
    }
    i16::try_from(rounded.coefficient)
        .map(|x| Value::Int(i32::from(x)))
        .map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "smallint out of range",
            ))
        })
}

pub(super) fn numeric_to_i32(v: &NumericValue) -> DbResult<Value> {
    numeric_to_i32_named(v, "integer")
}

pub(super) fn numeric_to_i32_named(v: &NumericValue, type_name: &str) -> DbResult<Value> {
    if v.is_special() {
        return Err(numeric_special_to_int_error(v, type_name));
    }
    // PostgreSQL rounds half-away-from-zero for numeric->int casts
    let rounded = v.round(0);
    if rounded.is_big() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            format!("{type_name} out of range"),
        )));
    }
    i32::try_from(rounded.coefficient)
        .map(Value::Int)
        .map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                format!("{type_name} out of range"),
            ))
        })
}

pub(super) fn numeric_to_i64(v: &NumericValue) -> DbResult<Value> {
    if v.is_special() {
        return Err(numeric_special_to_int_error(v, "bigint"));
    }
    // PostgreSQL rounds half-away-from-zero for numeric->int casts
    let rounded = v.round(0);
    if rounded.is_big() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "bigint out of range",
        )));
    }
    i64::try_from(rounded.coefficient)
        .map(Value::BigInt)
        .map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "bigint out of range",
            ))
        })
}

pub(crate) fn numeric_to_f64(v: &NumericValue) -> f64 {
    if v.is_nan() {
        return f64::NAN;
    }
    if v.is_pos_infinity() {
        return f64::INFINITY;
    }
    if v.is_neg_infinity() {
        return f64::NEG_INFINITY;
    }
    // Values whose coefficient does not fit in i128 store the magnitude in
    // the big-coefficient path; reading `coefficient` then yields 0.
    // `NumericValue::to_f64()` already implements the correct ±Inf / finite
    // conversion for the big case via `approx_f64_from_decimal_parts`,
    // operating on the abs-magnitude digit string instead of the full Display
    // form - same numerical result without the leading sign + decimal point
    // formatting cost or the subsequent `f64::from_str` parse.
    if v.is_big() {
        return v.to_f64();
    }
    if v.scale == 0 {
        i128_to_f64(v.coefficient)
    } else {
        let exp = to_i32_saturating(v.scale);
        i128_to_f64(v.coefficient) / 10f64.powi(exp)
    }
}

pub(crate) fn float_to_numeric(v: f64) -> DbResult<Value> {
    float_to_numeric_with_scale(v, None)
}

/// Convert an f64 to a Numeric value.
///
/// When `target_scale` is `Some(n)`, the result will have exactly `n` decimal
/// places (matching `PostgreSQL`'s behaviour for functions like sqrt, ln, log,
/// exp that return numeric with a computed result scale).
///
/// When `target_scale` is `None`, the f64 is rendered with full precision
/// (up to 17 significant digits - enough to round-trip any f64).
pub(crate) fn float_to_numeric_with_scale(v: f64, target_scale: Option<u32>) -> DbResult<Value> {
    if v.is_nan() {
        return Ok(Value::Numeric(NumericValue::NAN));
    }
    if v.is_infinite() {
        return Ok(Value::Numeric(if v > 0.0 {
            NumericValue::INFINITY
        } else {
            NumericValue::NEG_INFINITY
        }));
    }
    match target_scale {
        Some(scale) => {
            if scale > NUMERIC_MAX_DISPLAY_SCALE {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    format!(
                        "requested numeric scale {scale} exceeds maximum supported scale {NUMERIC_MAX_DISPLAY_SCALE}"
                    ),
                )));
            }
            let prec = usize::try_from(scale).map_err(|_| {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    format!("requested numeric scale {scale} exceeds platform precision limits"),
                ))
            })?;
            let s = format!("{v:.prec$}");
            s.parse::<NumericValue>()
                .map(Value::Numeric)
                .map_err(|_| DbError::invalid_input_syntax("numeric", &v.to_string()))
        }
        None => {
            // Use full precision representation (up to 17 significant digits)
            let s = format!("{v}");
            s.parse::<NumericValue>()
                .map(Value::Numeric)
                .map_err(|_| DbError::invalid_input_syntax("numeric", &v.to_string()))
        }
    }
}

/// Parse a `PostgreSQL` 16-style integer literal.
///
/// Supports:
/// - Plain decimal: `123`, `-456`
/// - Hex: `0x1A2F`, `0X1a2f`
/// - Octal: `0o777`, `0O777`
/// - Binary: `0b1010`, `0B1010`
/// - Underscore separators: `1_000_000`, `0xFF_FF`
///
/// Returns `Ok(value)` on success, `Err(Overflow)` if the value is valid
/// but out of i128 range, or `Err(Invalid)` if the syntax is wrong.
pub(crate) enum PgIntParseResult {
    Ok(i128),
    Overflow,
    Invalid,
}

pub(crate) fn parse_pg_int_literal(s: &str) -> PgIntParseResult {
    let t = s.trim();
    if t.is_empty() {
        return PgIntParseResult::Invalid;
    }

    // Handle optional leading sign (no space allowed between sign and digits)
    let (negative, body) = if let Some(rest) = t.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = t.strip_prefix('+') {
        (false, rest)
    } else {
        (false, t)
    };

    if body.is_empty() {
        return PgIntParseResult::Invalid;
    }

    // Determine base and strip prefix
    let (radix, digits) =
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16u32, hex)
        } else if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8u32, oct)
        } else if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2u32, bin)
        } else {
            (10u32, body)
        };

    // Validate and strip underscores.
    // PostgreSQL rules:
    // - No leading underscore (after any prefix)
    // - No trailing underscore
    // - No consecutive underscores
    // - Underscores only between valid digits
    if digits.is_empty() {
        return PgIntParseResult::Invalid;
    }

    // Check for leading underscore (only reject if it's NOT a non-decimal
    // base; for non-decimal, PostgreSQL allows a leading underscore after
    // the 0x/0o/0b prefix)
    if digits.starts_with('_') && radix == 10 {
        return PgIntParseResult::Invalid;
    }

    // Check for trailing underscore
    if digits.ends_with('_') {
        return PgIntParseResult::Invalid;
    }

    // Check for consecutive underscores
    if digits.contains("__") {
        return PgIntParseResult::Invalid;
    }

    let clean: String = digits.chars().filter(|c| *c != '_').collect();

    if clean.is_empty() {
        return PgIntParseResult::Invalid;
    }

    // Validate that all remaining chars are valid for the radix
    let all_valid = clean.chars().all(|c| c.is_digit(radix));
    if !all_valid {
        return PgIntParseResult::Invalid;
    }

    // Parse as u128 to handle the full unsigned range, then apply sign
    match u128::from_str_radix(&clean, radix) {
        Ok(uval) => {
            if negative {
                // Check if value fits in negative i128
                match uval.cmp(&(i128::MAX.cast_unsigned() + 1)) {
                    std::cmp::Ordering::Greater => PgIntParseResult::Overflow,
                    std::cmp::Ordering::Equal => PgIntParseResult::Ok(i128::MIN),
                    std::cmp::Ordering::Less => i128::try_from(uval)
                        .map_or(PgIntParseResult::Overflow, |value| {
                            PgIntParseResult::Ok(-value)
                        }),
                }
            } else if uval > i128::MAX.cast_unsigned() {
                PgIntParseResult::Overflow
            } else {
                i128::try_from(uval).map_or(PgIntParseResult::Overflow, PgIntParseResult::Ok)
            }
        }
        Err(_) => PgIntParseResult::Overflow,
    }
}

/// Decode a single hex character to its nibble value.
#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a pair of hex characters into a byte.
#[inline]
fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    Some(hex_nibble(hi)? << 4 | hex_nibble(lo)?)
}

/// Parse hex bytes from a hex string (used for BLOB).
pub(super) fn parse_hex_bytes(hex: &str) -> Result<Vec<u8>, ()> {
    // Strip spaces for PG compat (e.g., "De Ad Be Ef")
    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err(());
    }
    let bytes = hex.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        result.push(hex_pair(bytes[i], bytes[i + 1]).ok_or(())?);
        i += 2;
    }
    Ok(result)
}

/// Parse PG-style bytea escape sequences: \NNN for octal, plain text for ASCII.
pub(super) fn parse_bytea_escape(s: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            // Check for octal escape \NNN
            let d1 = bytes[i + 1];
            let d2 = bytes[i + 2];
            let d3 = bytes[i + 3];
            if (b'0'..=b'3').contains(&d1)
                && (b'0'..=b'7').contains(&d2)
                && (b'0'..=b'7').contains(&d3)
            {
                let val = (d1 - b'0') * 64 + (d2 - b'0') * 8 + (d3 - b'0');
                result.push(val);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    result
}
