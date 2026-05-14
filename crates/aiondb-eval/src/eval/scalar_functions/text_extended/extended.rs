use super::super::value_convert::to_i32_saturating;
use aiondb_core::{DbError, DbResult, Value};
use regex::Regex;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

use super::super::expect_arg_range;

#[inline]
fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[inline]
fn nonneg_i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[inline]
fn nonneg_i32_to_usize(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

// =====================================================================
// btrim - trim characters from both sides of a string
// =====================================================================

pub(crate) fn eval_btrim(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "btrim() requires 1 or 2 arguments")?;
    match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Blob(b) => {
            // bytea btrim
            let trim_bytes: &[u8] = if args.len() == 2 {
                match &args[1] {
                    Value::Blob(t) => t.as_slice(),
                    Value::Text(s) => s.as_bytes(),
                    Value::Null => return Ok(Value::Null),
                    _ => return Err(DbError::internal("btrim() second arg must be bytea")),
                }
            } else {
                b" "
            };
            let start = b
                .iter()
                .position(|byte| !trim_bytes.contains(byte))
                .unwrap_or(b.len());
            let end = b
                .iter()
                .rposition(|byte| !trim_bytes.contains(byte))
                .map_or(start, |p| p + 1);
            return Ok(Value::Blob(b[start..end].to_vec()));
        }
        _ => {}
    }
    let s_buf;
    let s: &str = match &args[0] {
        Value::Text(s) => s.as_str(),
        other => {
            s_buf = other.to_string();
            s_buf.as_str()
        }
    };
    let trim_chars_owned;
    let trim_chars: &str = if args.len() == 2 {
        match &args[1] {
            Value::Text(t) => t.as_str(),
            Value::Null => return Ok(Value::Null),
            other => {
                trim_chars_owned = other.to_string();
                trim_chars_owned.as_str()
            }
        }
    } else {
        " "
    };
    // ASCII fast path: build a 256-byte lookup table for the trim
    // character set and walk the input bytes. Same idiom as
    // trim_impl in text.rs; per-char `Vec<char>::contains` (O(N))
    // becomes an O(1) byte load.
    if s.is_ascii() && trim_chars.is_ascii() {
        let mut table = [false; 256];
        for &b in trim_chars.as_bytes() {
            table[b as usize] = true;
        }
        let bytes = s.as_bytes();
        let start = bytes
            .iter()
            .position(|b| !table[*b as usize])
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !table[*b as usize])
            .map_or(start, |p| p + 1);
        return Ok(Value::Text(
            std::str::from_utf8(&bytes[start..end])
                .unwrap_or("")
                .to_owned(),
        ));
    }
    // Slow path: chars-aware. The trim-char set is owned by
    // `trim_chars_set` regardless of source for a single predicate.
    let trim_chars_set: Vec<char> = trim_chars.chars().collect();
    let trimmed = s
        .trim_start_matches(|c| trim_chars_set.contains(&c))
        .trim_end_matches(|c| trim_chars_set.contains(&c));
    Ok(Value::Text(trimmed.to_owned()))
}

// =====================================================================
// unistr - Unicode escape string function
// =====================================================================

pub(crate) fn eval_unistr(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 1, "unistr() requires 1 argument")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => parse_unistr(s),
        other => parse_unistr(&other.to_string()),
    }
}

fn parse_unistr(input: &str) -> DbResult<Value> {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            if i + 1 >= chars.len() {
                return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
            }
            if chars[i + 1] == '\\' {
                // Escaped backslash
                result.push('\\');
                i += 2;
            } else if chars[i + 1] == '+' {
                // \+XXXXXX - 6-digit hex
                if i + 8 > chars.len() {
                    return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
                }
                let hex: String = chars[i + 2..i + 8].iter().collect();
                let cp = u32::from_str_radix(&hex, 16)
                    .map_err(|_| DbError::syntax_error("invalid Unicode escape in unistr"))?;
                validate_and_push_codepoint(cp, &mut result)?;
                i += 8;
            } else if (chars[i + 1] == 'u' || chars[i + 1] == 'U') && i + 2 < chars.len() {
                // \uXXXX or \UXXXXXXXX
                if chars[i + 1] == 'u' {
                    // \uXXXX - 4 hex digits
                    if i + 6 > chars.len() {
                        return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
                    }
                    let hex: String = chars[i + 2..i + 6].iter().collect();
                    let cp = u32::from_str_radix(&hex, 16)
                        .map_err(|_| DbError::syntax_error("invalid Unicode escape in unistr"))?;
                    validate_and_push_codepoint(cp, &mut result)?;
                    i += 6;
                } else {
                    // \UXXXXXXXX - 8 hex digits
                    if i + 10 > chars.len() {
                        return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
                    }
                    let hex: String = chars[i + 2..i + 10].iter().collect();
                    let cp = u32::from_str_radix(&hex, 16)
                        .map_err(|_| DbError::syntax_error("invalid Unicode escape in unistr"))?;
                    validate_and_push_codepoint(cp, &mut result)?;
                    i += 10;
                }
            } else if chars[i + 1].is_ascii_hexdigit() {
                // \XXXX - 4-digit hex (standard form)
                if i + 5 > chars.len() {
                    return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
                }
                let hex: String = chars[i + 1..i + 5].iter().collect();
                // Check all 4 are hex digits
                if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(DbError::syntax_error("invalid Unicode escape in unistr"));
                }
                let cp = u32::from_str_radix(&hex, 16)
                    .map_err(|_| DbError::syntax_error("invalid Unicode escape in unistr"))?;
                validate_and_push_codepoint(cp, &mut result)?;
                i += 5;
            } else {
                return Err(DbError::syntax_error(format!(
                    "invalid Unicode escape character \"{}\"",
                    chars[i + 1]
                )));
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    Ok(Value::Text(result))
}

fn validate_and_push_codepoint(cp: u32, result: &mut String) -> DbResult<()> {
    // Check for surrogates
    if (0xD800..=0xDFFF).contains(&cp) {
        return Err(DbError::syntax_error(format!(
            "invalid Unicode surrogate pair at code point {cp:04X}"
        )));
    }
    if cp > 0x10FFFF {
        return Err(DbError::syntax_error(format!(
            "invalid Unicode code point: {cp:X}"
        )));
    }
    let ch = char::from_u32(cp)
        .ok_or_else(|| DbError::syntax_error(format!("invalid Unicode code point: {cp:X}")))?;
    result.push(ch);
    Ok(())
}

// =====================================================================
// SHA-2 family functions
// =====================================================================

fn sha_input(args: &[Value], name: &str) -> DbResult<Vec<u8>> {
    expect_arg_range(args, 1, 1, &format!("{name}() requires 1 argument"))?;
    match &args[0] {
        Value::Null => Err(DbError::internal("null input")), // caller checks
        Value::Text(s) => Ok(s.as_bytes().to_vec()),
        Value::Blob(b) => Ok(b.clone()),
        other => Ok(other.to_string().into_bytes()),
    }
}

fn eval_sha_generic<D: Digest>(args: &[Value], name: &str) -> DbResult<Value> {
    if matches!(args.first(), Some(Value::Null)) {
        return Ok(Value::Null);
    }
    let input = sha_input(args, name)?;
    let result = D::digest(&input);
    Ok(Value::Blob(result.to_vec()))
}

pub(crate) fn eval_sha224(args: &[Value]) -> DbResult<Value> {
    eval_sha_generic::<Sha224>(args, "sha224")
}

pub(crate) fn eval_sha256(args: &[Value]) -> DbResult<Value> {
    eval_sha_generic::<Sha256>(args, "sha256")
}

pub(crate) fn eval_sha384(args: &[Value]) -> DbResult<Value> {
    eval_sha_generic::<Sha384>(args, "sha384")
}

pub(crate) fn eval_sha512(args: &[Value]) -> DbResult<Value> {
    eval_sha_generic::<Sha512>(args, "sha512")
}

// =====================================================================
// Bytea bit/byte manipulation functions
// =====================================================================

pub(crate) fn eval_get_bit(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 2, "get_bit() requires 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let bytes = match &args[0] {
        Value::Blob(b) => b.as_slice(),
        _ => return Err(DbError::internal("get_bit() first arg must be bytea")),
    };
    let n = extract_int64(&args[1], "get_bit() second arg must be integer")?;
    validate_index_range(
        n,
        usize_to_i64_saturating(bytes.len())
            .saturating_mul(8)
            .saturating_sub(1),
    )?;
    let byte_idx = nonneg_i64_to_usize(n / 8);
    let bit_idx = u32::try_from(n % 8).unwrap_or(0);
    let bit = (bytes[byte_idx] >> (7 - bit_idx)) & 1;
    Ok(Value::Int(i32::from(bit)))
}

pub(crate) fn eval_set_bit(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 3, 3, "set_bit() requires 3 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let bytes = match &args[0] {
        Value::Blob(b) => b.clone(),
        _ => return Err(DbError::internal("set_bit() first arg must be bytea")),
    };
    let n = extract_int64(&args[1], "set_bit() second arg must be integer")?;
    let new_val = extract_int(&args[2], "set_bit", "third")
        .map_err(|_| DbError::internal("set_bit() third arg must be integer"))?;
    validate_index_range(
        n,
        usize_to_i64_saturating(bytes.len())
            .saturating_mul(8)
            .saturating_sub(1),
    )?;
    let mut result = bytes;
    let byte_idx = nonneg_i64_to_usize(n / 8);
    let bit_idx = u32::try_from(n % 8).unwrap_or(0);
    if new_val != 0 {
        result[byte_idx] |= 1 << (7 - bit_idx);
    } else {
        result[byte_idx] &= !(1 << (7 - bit_idx));
    }
    Ok(Value::Blob(result))
}

pub(crate) fn eval_get_byte(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 2, "get_byte() requires 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let bytes = match &args[0] {
        Value::Blob(b) => b.as_slice(),
        _ => return Err(DbError::internal("get_byte() first arg must be bytea")),
    };
    let n = extract_int64(&args[1], "get_byte() second arg must be integer")?;
    validate_index_range(n, usize_to_i64_saturating(bytes.len()).saturating_sub(1))?;
    Ok(Value::Int(i32::from(bytes[nonneg_i64_to_usize(n)])))
}

pub(crate) fn eval_set_byte(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 3, 3, "set_byte() requires 3 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let bytes = match &args[0] {
        Value::Blob(b) => b.clone(),
        _ => return Err(DbError::internal("set_byte() first arg must be bytea")),
    };
    let n = extract_int64(&args[1], "set_byte() second arg must be integer")?;
    let raw = extract_int64(&args[2], "set_byte() third arg must be integer")?;
    let new_val = u8::try_from(raw & 0xFF).unwrap_or(0);
    validate_index_range(n, usize_to_i64_saturating(bytes.len()).saturating_sub(1))?;
    let mut result = bytes;
    result[nonneg_i64_to_usize(n)] = new_val;
    Ok(Value::Blob(result))
}

pub(crate) fn eval_bit_count(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 1, "bit_count() requires 1 argument")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Blob(b) => {
            let count: u32 = b.iter().map(|byte| byte.count_ones()).sum();
            Ok(Value::BigInt(i64::from(count)))
        }
        _ => Err(DbError::internal("bit_count() requires a bytea argument")),
    }
}

// =====================================================================
// regexp_count - count matches of pattern in string
// =====================================================================

/// Build a regex with PG flags, going through the per-thread regex cache so
/// that repeated `regexp_count(col, 'pat', 'i')` over a long scan only pays
/// the compile cost once. Returns a thread-shared `Arc<Regex>`.
fn build_pg_regex(pattern: &str, flags: &str) -> Result<std::sync::Arc<Regex>, DbError> {
    const MAX_REGEX_PATTERN_LEN: usize = 32_768;
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(DbError::internal(format!(
            "regular expression pattern too long ({} bytes, maximum is {MAX_REGEX_PATTERN_LEN})",
            pattern.len()
        )));
    }
    crate::regex_cache::get_pg_flags(pattern, flags)
        .map_err(|e| DbError::internal(format!("invalid regular expression: {e}")))
}

fn extract_text<'a>(val: &'a Value, name: &str, pos: &str) -> DbResult<&'a str> {
    match val {
        Value::Text(s) => Ok(s.as_str()),
        _ => Err(DbError::internal(format!(
            "{name}() {pos} arg must be text"
        ))),
    }
}

fn extract_int(val: &Value, name: &str, pos: &str) -> DbResult<i32> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::BigInt(n) => {
            i32::try_from(*n).map_err(|_| DbError::out_of_range("integer", &n.to_string()))
        }
        _ => Err(DbError::internal(format!(
            "{name}() {pos} arg must be integer"
        ))),
    }
}

fn extract_int64(val: &Value, message: &str) -> DbResult<i64> {
    match val {
        Value::Int(n) => Ok(i64::from(*n)),
        Value::BigInt(n) => Ok(*n),
        _ => Err(DbError::internal(message)),
    }
}

fn validate_index_range(index: i64, upper_bound: i64) -> DbResult<()> {
    if index < 0 || index > upper_bound {
        return Err(DbError::internal(format!(
            "index {index} out of valid range, 0..{upper_bound}"
        )));
    }
    Ok(())
}

pub(crate) fn eval_regexp_count(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 4, "regexp_count() requires 2 to 4 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = extract_text(&args[0], "regexp_count", "first")?;
    let pattern = extract_text(&args[1], "regexp_count", "second")?;

    let start = if args.len() >= 3 {
        let s = extract_int(&args[2], "regexp_count", "third")?;
        if s <= 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"start\": {s}"
            )));
        }
        nonneg_i32_to_usize(s)
    } else {
        1
    };

    let flags = if args.len() >= 4 {
        extract_text(&args[3], "regexp_count", "fourth")?
    } else {
        ""
    };

    // Validate flags - 'g' not valid for regexp_count
    for ch in flags.chars() {
        if !matches!(ch, 'i' | 'c' | 'n' | 'm' | 's' | 'x' | 'p' | 'q') {
            return Err(DbError::internal(format!(
                "invalid regular expression option: \"{ch}\""
            )));
        }
    }

    let re = build_pg_regex(pattern, flags)?;

    // Convert character position to byte position
    let byte_start = char_to_byte_offset(source, start - 1);
    let search_str = &source[byte_start..];

    let count = re.find_iter(search_str).count();
    Ok(Value::Int(to_i32_saturating(count)))
}

// =====================================================================
// regexp_like - boolean match test
// =====================================================================

pub(crate) fn eval_regexp_like(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "regexp_like() requires 2 or 3 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = extract_text(&args[0], "regexp_like", "first")?;
    let pattern = extract_text(&args[1], "regexp_like", "second")?;

    let flags = if args.len() == 3 {
        extract_text(&args[2], "regexp_like", "third")?
    } else {
        ""
    };

    // 'g' flag is invalid for regexp_like
    if flags.contains('g') {
        return Err(DbError::internal(
            "regexp_like() does not support the \"global\" option",
        ));
    }

    let re = build_pg_regex(pattern, flags)?;
    Ok(Value::Boolean(re.is_match(source)))
}

// =====================================================================
// regexp_instr - return position of pattern match
// =====================================================================

pub(crate) fn eval_regexp_instr(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 7, "regexp_instr() requires 2 to 7 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = extract_text(&args[0], "regexp_instr", "first")?;
    let pattern = extract_text(&args[1], "regexp_instr", "second")?;

    let start = if args.len() >= 3 {
        let s = extract_int(&args[2], "regexp_instr", "third")?;
        if s <= 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"start\": {s}"
            )));
        }
        nonneg_i32_to_usize(s)
    } else {
        1
    };

    let occurrence = if args.len() >= 4 {
        let n = extract_int(&args[3], "regexp_instr", "fourth")?;
        if n <= 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"n\": {n}"
            )));
        }
        nonneg_i32_to_usize(n)
    } else {
        1
    };

    let return_end = if args.len() >= 5 {
        let e = extract_int(&args[4], "regexp_instr", "fifth")?;
        if e != 0 && e != 1 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"endoption\": {e}"
            )));
        }
        e == 1
    } else {
        false
    };

    let flags = if args.len() >= 6 {
        let f = extract_text(&args[5], "regexp_instr", "sixth")?;
        if f.contains('g') {
            return Err(DbError::internal(
                "regexp_instr() does not support the \"global\" option",
            ));
        }
        f
    } else {
        ""
    };

    let subexpr = if args.len() >= 7 {
        let s = extract_int(&args[6], "regexp_instr", "seventh")?;
        if s < 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"subexpr\": {s}"
            )));
        }
        nonneg_i32_to_usize(s)
    } else {
        0
    };

    let re = build_pg_regex(pattern, flags)?;

    // Convert character position to byte position
    let byte_start = char_to_byte_offset(source, start - 1);
    let search_str = &source[byte_start..];

    let mut match_count = 0;
    for caps in re.captures_iter(search_str) {
        match_count += 1;
        if match_count == occurrence {
            if subexpr > 0 {
                // Return position of subexpression
                match caps.get(subexpr) {
                    Some(m) => {
                        let pos = if return_end {
                            byte_to_char_offset(source, byte_start + m.end()) + 1
                        } else {
                            byte_to_char_offset(source, byte_start + m.start()) + 1
                        };
                        return Ok(Value::Int(to_i32_saturating(pos)));
                    }
                    None => return Ok(Value::Int(0)),
                }
            }
            // Return position of whole match
            let Some(m) = caps.get(0) else {
                continue;
            };
            let pos = if return_end {
                byte_to_char_offset(source, byte_start + m.end()) + 1
            } else {
                byte_to_char_offset(source, byte_start + m.start()) + 1
            };
            return Ok(Value::Int(to_i32_saturating(pos)));
        }
    }
    Ok(Value::Int(0))
}

// =====================================================================
// regexp_substr - extract substring matching pattern
// =====================================================================

pub(crate) fn eval_regexp_substr(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 6, "regexp_substr() requires 2 to 6 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = extract_text(&args[0], "regexp_substr", "first")?;
    let pattern = extract_text(&args[1], "regexp_substr", "second")?;

    let start = if args.len() >= 3 {
        let s = extract_int(&args[2], "regexp_substr", "third")?;
        if s <= 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"start\": {s}"
            )));
        }
        nonneg_i32_to_usize(s)
    } else {
        1
    };

    let occurrence = if args.len() >= 4 {
        let n = extract_int(&args[3], "regexp_substr", "fourth")?;
        if n <= 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"n\": {n}"
            )));
        }
        nonneg_i32_to_usize(n)
    } else {
        1
    };

    let flags = if args.len() >= 5 {
        let f = extract_text(&args[4], "regexp_substr", "fifth")?;
        if f.contains('g') {
            return Err(DbError::internal(
                "regexp_substr() does not support the \"global\" option",
            ));
        }
        f
    } else {
        ""
    };

    let subexpr = if args.len() >= 6 {
        let s = extract_int(&args[5], "regexp_substr", "sixth")?;
        if s < 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"subexpr\": {s}"
            )));
        }
        nonneg_i32_to_usize(s)
    } else {
        0
    };

    let re = build_pg_regex(pattern, flags)?;

    let byte_start = char_to_byte_offset(source, start - 1);
    let search_str = &source[byte_start..];

    let mut match_count = 0;
    for caps in re.captures_iter(search_str) {
        match_count += 1;
        if match_count == occurrence {
            if subexpr > 0 {
                match caps.get(subexpr) {
                    Some(m) => return Ok(Value::Text(m.as_str().to_string())),
                    None => return Ok(Value::Null),
                }
            }
            let Some(m) = caps.get(0) else {
                continue;
            };
            return Ok(Value::Text(m.as_str().to_string()));
        }
    }
    Ok(Value::Null)
}

// =====================================================================
// Character/byte offset conversion helpers
// =====================================================================

/// Convert a character offset (0-based) to a byte offset in a UTF-8 string.
fn char_to_byte_offset(s: &str, char_offset: usize) -> usize {
    s.char_indices()
        .nth(char_offset)
        .map_or(s.len(), |(byte_pos, _)| byte_pos)
}

/// Convert a byte offset to a character offset (0-based) in a UTF-8 string.
fn byte_to_char_offset(s: &str, byte_offset: usize) -> usize {
    s[..byte_offset].chars().count()
}
