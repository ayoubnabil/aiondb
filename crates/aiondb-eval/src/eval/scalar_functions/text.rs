use super::value_convert::{checked_usize_to_i32, coerce_i32_like, expect_i32_value};
use super::*;

#[inline]
fn nonneg_i32_to_usize(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

pub(super) fn eval_upper(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "upper")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            if looks_like_range(s) {
                return super::range::eval_range_upper(args);
            }
            if super::range::looks_like_multirange(s) {
                return super::range::eval_multirange_upper(args);
            }
            // `to_uppercase` always walks each `char` through the full
            // Unicode case-fold table, which is roughly an order of
            // magnitude slower than the ASCII byte mapping that PG
            // applies to ASCII-only text by default. Detect ASCII
            // input and route to `make_ascii_uppercase` on a single
            // allocation, leaving the Unicode path untouched for non-
            // ASCII strings.
            let trimmed = case_mapping_input(s);
            let result = if trimmed.is_ascii() {
                let mut out = trimmed.to_owned();
                out.make_ascii_uppercase();
                out
            } else {
                trimmed.to_uppercase()
            };
            Ok(Value::Text(result))
        }
        _ => Err(DbError::internal("upper() requires a text argument")),
    }
}

pub(super) fn eval_lower(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lower")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            if looks_like_range(s) {
                return super::range::eval_range_lower(args);
            }
            if super::range::looks_like_multirange(s) {
                return super::range::eval_multirange_lower(args);
            }
            let trimmed = case_mapping_input(s);
            let result = if trimmed.is_ascii() {
                let mut out = trimmed.to_owned();
                out.make_ascii_lowercase();
                out
            } else {
                trimmed.to_lowercase()
            };
            Ok(Value::Text(result))
        }
        _ => Err(DbError::internal("lower() requires a text argument")),
    }
}

fn case_mapping_input(s: &str) -> &str {
    s.trim_end_matches(' ')
}

/// Check if a text value looks like a PostgreSQL range literal.
fn looks_like_range(s: &str) -> bool {
    let s = s.trim();
    if s.eq_ignore_ascii_case("empty") {
        return true;
    }
    if s.len() < 3 {
        return false;
    }
    let first = s.as_bytes()[0];
    let last = s.as_bytes()[s.len() - 1];
    (first == b'[' || first == b'(') && (last == b']' || last == b')')
}

pub(super) fn eval_length(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "length")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        // For ASCII strings the character count equals the byte length
        // and we can read it without iterating; only fall back to the
        // O(N) `chars().count()` walk for genuinely multi-byte UTF-8.
        Value::Text(s) => {
            let count = if s.is_ascii() {
                s.len()
            } else {
                s.chars().count()
            };
            Ok(Value::Int(checked_usize_to_i32(
                count,
                "length() result exceeds INT range",
            )?))
        }
        Value::Blob(b) => Ok(Value::Int(checked_usize_to_i32(
            b.len(),
            "length() result exceeds INT range",
        )?)),
        Value::Array(arr) => Ok(Value::Int(checked_usize_to_i32(
            arr.len(),
            "length() result exceeds INT range",
        )?)),
        other => {
            let s = other.to_string();
            let count = if s.is_ascii() {
                s.len()
            } else {
                s.chars().count()
            };
            Ok(Value::Int(checked_usize_to_i32(
                count,
                "length() result exceeds INT range",
            )?))
        }
    }
}

pub(super) fn eval_octet_length(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "octet_length")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::Int(checked_usize_to_i32(
            s.len(),
            "octet_length() result exceeds INT range",
        )?)),
        Value::Blob(b) => Ok(Value::Int(checked_usize_to_i32(
            b.len(),
            "octet_length() result exceeds INT range",
        )?)),
        other => {
            let s = other.to_string();
            Ok(Value::Int(checked_usize_to_i32(
                s.len(),
                "octet_length() result exceeds INT range",
            )?))
        }
    }
}

pub(super) fn eval_substring(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || args.len() > 3 {
        return Err(DbError::internal("substring() requires 1 to 3 arguments"));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let text_buf;
    let s = match &args[0] {
        Value::Text(s) => s.as_str(),
        Value::Blob(b) => {
            text_buf = String::from_utf8_lossy(b).into_owned();
            text_buf.as_str()
        }
        other => {
            text_buf = other.to_string();
            text_buf.as_str()
        }
    };
    // Single-arg form: substring(string) - return entire string
    if args.len() == 1 {
        return Ok(Value::Text(s.to_string()));
    }
    // Try to interpret second arg as integer position; if it's a non-numeric
    // text value (i.e. a regex pattern), return NULL gracefully.
    let start = match &args[1] {
        Value::Null => return Ok(Value::Null),
        Value::Text(t) => match t.parse::<i32>() {
            Ok(n) => n,
            Err(_) if args.len() == 2 => return substring_regex_match(s, t),
            Err(_) => return Ok(Value::Null),
        },
        // BigInt: clamp to i32 range (PostgreSQL treats extreme positions
        // as very-far-before / very-far-after the string).
        Value::BigInt(n) => i32::try_from(*n).unwrap_or(if *n < 0 { i32::MIN } else { i32::MAX }),
        other => match coerce_i32_like(other) {
            Some(value) => value,
            None => return Ok(Value::Null),
        },
    };
    // PostgreSQL: 1-based, start < 1 reduces the length
    let zero_based = nonneg_i32_to_usize(start.saturating_sub(1).max(0));
    let len_arg = if args.len() == 3 {
        let len = match &args[2] {
            Value::Null => return Ok(Value::Null),
            Value::Text(s) => s
                .parse::<i32>()
                .map_err(|_| DbError::internal("substring() third arg must be integer"))?,
            Value::BigInt(n) => {
                i32::try_from(*n).unwrap_or(if *n < 0 { i32::MIN } else { i32::MAX })
            }
            other => coerce_i32_like(other)
                .ok_or_else(|| DbError::internal("substring() third arg must be integer"))?,
        };
        if len < 0 {
            return Err(DbError::internal("negative substring length not allowed"));
        }
        // PostgreSQL: if start < 1, effective length is reduced
        let effective_len = if start < 1 {
            i64::from(len)
                .saturating_add(i64::from(start))
                .saturating_sub(1)
        } else {
            i64::from(len)
        };
        Some(effective_len)
    } else {
        None
    };
    // ASCII fast path: char index == byte index, so we can slice the
    // input string without ever materialising a `Vec<char>`. The slow
    // fallback collects characters because non-ASCII strings can have
    // multi-byte chars whose width != index.
    let result: String = if s.is_ascii() {
        let bytes = s.as_bytes();
        let total = bytes.len();
        let begin = zero_based.min(total);
        let end = match len_arg {
            None => total,
            Some(n) if n <= 0 => begin,
            Some(n) => begin
                .saturating_add(usize::try_from(n).unwrap_or(usize::MAX))
                .min(total),
        };
        // SAFETY-equivalent (no unsafe): begin and end are byte indices
        // computed from `total = bytes.len()`, and we only enter this
        // branch when the input is ASCII, so they are guaranteed
        // char-boundaries.
        s[begin..end].to_owned()
    } else {
        // Stream chars directly into the result so multi-byte input
        // doesn't pay the Vec<char> intermediate allocation.
        match len_arg {
            None => s.chars().skip(zero_based).collect(),
            Some(n) if n <= 0 => String::new(),
            Some(n) => s
                .chars()
                .skip(zero_based)
                .take(usize::try_from(n).unwrap_or(usize::MAX))
                .collect(),
        }
    };
    Ok(Value::Text(result))
}

fn substring_regex_match(input: &str, pattern: &str) -> DbResult<Value> {
    // Compile via the per-thread regex cache so a SELECT substring(col,
    // 'pattern') over millions of rows pays a single compile, not one per
    // row.
    let regex = crate::regex_cache::get(pattern)
        .map_err(|_| DbError::internal("substring() invalid regular expression"))?;
    let Some(captures) = regex.captures(input) else {
        return Ok(Value::Null);
    };
    if captures.len() > 1 {
        Ok(Value::Text(
            captures
                .get(1)
                .map(|capture| capture.as_str().to_owned())
                .unwrap_or_default(),
        ))
    } else {
        Ok(Value::Text(
            captures
                .get(0)
                .map(|capture| capture.as_str().to_owned())
                .unwrap_or_default(),
        ))
    }
}

pub(super) fn eval_trim(args: &[Value]) -> DbResult<Value> {
    trim_impl(args, "trim()", TrimSide::Both)
}

pub(super) fn eval_ltrim(args: &[Value]) -> DbResult<Value> {
    trim_impl(args, "ltrim()", TrimSide::Start)
}

pub(super) fn eval_rtrim(args: &[Value]) -> DbResult<Value> {
    trim_impl(args, "rtrim()", TrimSide::End)
}

#[derive(Copy, Clone)]
enum TrimSide {
    Both,
    Start,
    End,
}

fn trim_impl(args: &[Value], name: &str, side: TrimSide) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, &format!("{name} requires 1 or 2 arguments"))?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => trim_text(s, args, name, side),
        // Trim is a textual op; coerce non-Text inputs through their
        // Display-form.
        other => {
            let buf = other.to_string();
            trim_text(&buf, args, name, side)
        }
    }
}

fn trim_text(s: &str, args: &[Value], name: &str, side: TrimSide) -> DbResult<Value> {
    if args.len() == 2 {
        if args[1].is_null() {
            return Ok(Value::Null);
        }
        let trim_chars = expect_text_arg(args, 1, name, "second")?;
        // ASCII fast path: build a 256-byte lookup table for the trim
        // character set so membership tests are O(1) byte loads
        // instead of `Vec<char>::contains` (O(N) scan), and skip the
        // per-call `chars().collect()` Vec<char> allocation entirely.
        // Non-ASCII characters in either operand fall back to the
        // chars-based predicate so multi-byte sequences stay correct.
        if s.is_ascii() && trim_chars.is_ascii() {
            let mut table = [false; 256];
            for &b in trim_chars.as_bytes() {
                table[b as usize] = true;
            }
            let trimmed = match side {
                TrimSide::Both => trim_ascii_both(s.as_bytes(), &table),
                TrimSide::Start => trim_ascii_start(s.as_bytes(), &table),
                TrimSide::End => trim_ascii_end(s.as_bytes(), &table),
            };
            return Ok(Value::Text(
                std::str::from_utf8(trimmed).unwrap_or("").to_owned(),
            ));
        }
        let chars_set: Vec<char> = trim_chars.chars().collect();
        let predicate = |c: char| chars_set.contains(&c);
        let trimmed = match side {
            TrimSide::Both => s.trim_matches(predicate),
            TrimSide::Start => s.trim_start_matches(predicate),
            TrimSide::End => s.trim_end_matches(predicate),
        };
        Ok(Value::Text(trimmed.to_owned()))
    } else {
        let trimmed = match side {
            TrimSide::Both => s.trim(),
            TrimSide::Start => s.trim_start(),
            TrimSide::End => s.trim_end(),
        };
        Ok(Value::Text(trimmed.to_owned()))
    }
}

#[inline]
fn trim_ascii_start<'a>(bytes: &'a [u8], table: &[bool; 256]) -> &'a [u8] {
    let start = bytes
        .iter()
        .position(|b| !table[*b as usize])
        .unwrap_or(bytes.len());
    &bytes[start..]
}

#[inline]
fn trim_ascii_end<'a>(bytes: &'a [u8], table: &[bool; 256]) -> &'a [u8] {
    let end = bytes
        .iter()
        .rposition(|b| !table[*b as usize])
        .map_or(0, |p| p + 1);
    &bytes[..end]
}

#[inline]
fn trim_ascii_both<'a>(bytes: &'a [u8], table: &[bool; 256]) -> &'a [u8] {
    trim_ascii_end(trim_ascii_start(bytes, table), table)
}

pub(super) fn eval_replace(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "replace")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match (&args[0], &args[1], &args[2]) {
        (Value::Text(s), Value::Text(from), Value::Text(to)) => {
            Ok(Value::Text(s.replace(from.as_str(), to.as_str())))
        }
        _ => Err(DbError::internal("replace() requires three text arguments")),
    }
}

pub(super) fn eval_strpos(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "strpos")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match (&args[0], &args[1]) {
        (Value::Text(haystack), Value::Text(needle)) => {
            Ok(Value::Int(strpos_one_based(haystack, needle)?))
        }
        _ => Err(DbError::internal("strpos() requires two text arguments")),
    }
}

/// Common 1-based-character lookup shared by `strpos()` and `position()`.
/// On ASCII haystacks the byte offset returned by `find` is also the
/// character offset, so we skip the per-call `.chars().count()` walk.
#[inline]
fn strpos_one_based(haystack: &str, needle: &str) -> DbResult<i32> {
    haystack.find(needle).map_or(Ok(0), |idx| {
        let char_count = if haystack.is_ascii() {
            idx
        } else {
            haystack[..idx].chars().count()
        };
        checked_usize_to_i32(char_count, "strpos() result exceeds INT range")
            .map(|n| n.saturating_add(1))
    })
}

pub(super) fn eval_left(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "left")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "left()", "first")?;
    let n = expect_i32_value(&args[1], "left() second arg must be integer")?;
    // ASCII fast path: char count == byte count, so we slice the
    // input by bytes and bypass the chars() iterator entirely. The
    // chars-based path stays for non-ASCII so multi-byte characters
    // keep their boundaries.
    if s.is_ascii() {
        let bytes = s.as_bytes();
        let total = checked_usize_to_i32(bytes.len(), "left() input exceeds INT range")?;
        let take = if n < 0 {
            nonneg_i32_to_usize(total.saturating_add(n).max(0))
        } else {
            nonneg_i32_to_usize(n).min(bytes.len())
        };
        return Ok(Value::Text(s[..take].to_owned()));
    }
    if n < 0 {
        // PostgreSQL: left(s, -n) returns all but last n chars
        let total = checked_usize_to_i32(s.chars().count(), "left() input exceeds INT range")?;
        let take = nonneg_i32_to_usize(total.saturating_add(n).max(0));
        Ok(Value::Text(s.chars().take(take).collect()))
    } else {
        Ok(Value::Text(
            s.chars().take(nonneg_i32_to_usize(n)).collect(),
        ))
    }
}

pub(super) fn eval_right(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "right")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "right()", "first")?;
    let n = expect_i32_value(&args[1], "right() second arg must be integer")?;
    // ASCII fast path: byte slicing is char slicing; skip the
    // `Vec<char> = s.chars().collect()` allocation and the second
    // `chars[skip..].iter().collect()` walk that the previous path
    // paid even for short suffixes.
    if s.is_ascii() {
        let bytes = s.as_bytes();
        let total = checked_usize_to_i32(bytes.len(), "right() input exceeds INT range")?;
        let skip = if n < 0 {
            nonneg_i32_to_usize(n.saturating_neg().min(total))
        } else {
            nonneg_i32_to_usize(total.saturating_sub(n).max(0))
        };
        let skip = skip.min(bytes.len());
        return Ok(Value::Text(s[skip..].to_owned()));
    }
    let chars: Vec<char> = s.chars().collect();
    let total = checked_usize_to_i32(chars.len(), "right() input exceeds INT range")?;
    if n < 0 {
        // PostgreSQL: right(s, -n) returns all but first n chars
        let skip = nonneg_i32_to_usize(n.saturating_neg().min(total));
        Ok(Value::Text(chars[skip..].iter().collect()))
    } else {
        let skip = nonneg_i32_to_usize(total.saturating_sub(n).max(0));
        Ok(Value::Text(chars[skip..].iter().collect()))
    }
}

pub(super) fn eval_repeat(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "repeat")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "repeat()", "first")?;
    let n = expect_i32_value(&args[1], "repeat() second arg must be integer")?;
    if n <= 0 {
        Ok(Value::Text(String::new()))
    } else {
        const MAX_REPEAT_BYTES: usize = 10_000_000;
        let repeats = nonneg_i32_to_usize(n);
        let result_len = s.len().saturating_mul(repeats);
        if result_len > MAX_REPEAT_BYTES {
            return Err(DbError::internal(
                "repeat() result exceeds maximum allowed size (10 MB)",
            ));
        }
        Ok(Value::Text(s.repeat(repeats)))
    }
}

pub(super) fn eval_reverse(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "reverse")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            // For ASCII (the dominant shape), each char is one byte and
            // reversing bytes is identical to reversing chars. Skip the
            // `chars()` iterator and `String::from_iter<char>` machinery
            // and just walk the byte slice in reverse into a pre-sized
            // Vec<u8>. Non-ASCII strings need the chars()-aware
            // reverse so multi-byte sequences stay intact.
            if s.is_ascii() {
                let mut out = Vec::with_capacity(s.len());
                out.extend(s.as_bytes().iter().rev().copied());
                Ok(Value::Text(String::from_utf8(out).unwrap_or_default()))
            } else {
                Ok(Value::Text(s.chars().rev().collect()))
            }
        }
        _ => Err(DbError::internal("reverse() requires a text argument")),
    }
}

pub(super) fn eval_starts_with(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "starts_with")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match (&args[0], &args[1]) {
        (Value::Text(s), Value::Text(prefix)) => Ok(Value::Boolean(s.starts_with(prefix.as_str()))),
        _ => Err(DbError::internal(
            "starts_with() requires two text arguments",
        )),
    }
}

pub(super) fn eval_lpad(args: &[Value]) -> DbResult<Value> {
    pad_impl(args, "lpad()", PadSide::Left)
}

pub(super) fn eval_rpad(args: &[Value]) -> DbResult<Value> {
    pad_impl(args, "rpad()", PadSide::Right)
}

#[derive(Copy, Clone)]
enum PadSide {
    Left,
    Right,
}

fn pad_impl(args: &[Value], name: &str, side: PadSide) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, &format!("{name} requires 2 or 3 arguments"))?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, name, "first")?;
    let len = expect_i32_value(&args[1], &format!("{name} second arg must be integer"))?;
    let fill = if args.len() == 3 {
        expect_text_arg(args, 2, name, "third")?
    } else {
        " "
    };
    const MAX_PAD_LENGTH: usize = 10_000_000;
    if len <= 0 {
        return Ok(Value::Text(String::new()));
    }
    let len = nonneg_i32_to_usize(len);
    if len > MAX_PAD_LENGTH {
        return Err(DbError::internal(format!(
            "{name} result exceeds maximum allowed size (10M chars)"
        )));
    }
    // ASCII fast path for both string and fill: char positions equal
    // byte positions, so we can slice and pad through byte-level
    // operations and skip the per-call `s.chars().count()`,
    // `s.chars().take(len).collect()`, and `fill.chars().collect()`
    // walks. The non-ASCII case keeps the chars-based logic.
    if s.is_ascii() && fill.is_ascii() {
        let s_bytes = s.as_bytes();
        let fill_bytes = fill.as_bytes();
        if s_bytes.len() >= len {
            return Ok(Value::Text(s[..len].to_owned()));
        }
        if fill_bytes.is_empty() {
            return Ok(Value::Text(s.to_owned()));
        }
        let pad_needed = len - s_bytes.len();
        let mut out = Vec::with_capacity(len);
        match side {
            PadSide::Left => {
                for i in 0..pad_needed {
                    out.push(fill_bytes[i % fill_bytes.len()]);
                }
                out.extend_from_slice(s_bytes);
            }
            PadSide::Right => {
                out.extend_from_slice(s_bytes);
                for i in 0..pad_needed {
                    out.push(fill_bytes[i % fill_bytes.len()]);
                }
            }
        }
        return Ok(Value::Text(String::from_utf8(out).unwrap_or_default()));
    }
    // Slow path: chars-aware for inputs that may contain multi-byte
    // characters. Unchanged from the previous implementation.
    let char_count = s.chars().count();
    if char_count >= len {
        return Ok(Value::Text(s.chars().take(len).collect()));
    }
    let pad_needed = len - char_count;
    let fill_chars: Vec<char> = fill.chars().collect();
    if fill_chars.is_empty() {
        return Ok(Value::Text(s.to_string()));
    }
    let mut result = String::with_capacity(len);
    match side {
        PadSide::Left => {
            for i in 0..pad_needed {
                result.push(fill_chars[i % fill_chars.len()]);
            }
            result.push_str(s);
        }
        PadSide::Right => {
            result.push_str(s);
            for i in 0..pad_needed {
                result.push(fill_chars[i % fill_chars.len()]);
            }
        }
    }
    Ok(Value::Text(result))
}

pub(super) fn eval_position(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "position")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // position(substring, string) - same as strpos(string, substring)
    // In PostgreSQL: position(substring IN string), but as a function call
    // we follow strpos convention: position(haystack, needle)
    match (&args[0], &args[1]) {
        (Value::Text(haystack), Value::Text(needle)) => {
            Ok(Value::Int(strpos_one_based(haystack, needle)?))
        }
        _ => Err(DbError::internal("position() requires two text arguments")),
    }
}

pub(super) fn eval_concat_func(args: &[Value]) -> Value {
    // concat() skips NULLs and concatenates the rest. For the dominant
    // shapes (Text and small integers) we can `push_str` straight from
    // the borrowed contents instead of paying a per-arg
    // `value_to_text` String allocation that would just be copied into
    // the output buffer and dropped. Less-common variants still go
    // through value_to_text.
    use std::fmt::Write as _;
    let mut result = String::with_capacity(args.len() * 8);
    for arg in args {
        match arg {
            Value::Null => {}
            Value::Text(s) => result.push_str(s),
            Value::Int(n) => {
                let _ = write!(result, "{n}");
            }
            Value::BigInt(n) => {
                let _ = write!(result, "{n}");
            }
            Value::Boolean(true) => result.push('t'),
            Value::Boolean(false) => result.push('f'),
            other => result.push_str(&super::value_to_text(other)),
        }
    }
    Value::Text(result)
}
