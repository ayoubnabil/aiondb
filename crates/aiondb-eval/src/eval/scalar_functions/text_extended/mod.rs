#![allow(clippy::doc_markdown)]

use std::borrow::Cow;

use super::value_convert::to_i32_saturating;
use aiondb_core::{DbError, DbResult, Value};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as Base64Engine};
use md5::{Digest, Md5};
use regex::Regex;

use super::{expect_arg_range, expect_args, expect_text_arg};

fn jsonb_to_cypher_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::BigInt(i)
            } else if let Some(f) = n.as_f64() {
                Value::Double(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        // Containers stay as JSONB; Cypher comparisons that need them
        // unwrapped go through dedicated helpers.
        _ => Value::Jsonb(v.clone()),
    }
}

#[inline]
fn nonneg_i32_to_usize(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn expect_i32_value(value: &Value, message: &str) -> DbResult<i32> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::BigInt(n) => {
            i32::try_from(*n).map_err(|_| DbError::out_of_range("integer", &n.to_string()))
        }
        _ => Err(DbError::internal(message)),
    }
}

fn parse_regexp_replace_fourth_arg(value: &Value) -> DbResult<(i32, Option<i32>, Cow<'_, str>)> {
    match value {
        Value::Text(s) => Ok((1, None, Cow::Borrowed(s.as_str()))),
        _ => Ok((
            expect_i32_value(
                value,
                "regexp_replace() fourth arg must be text (flags) or integer (start)",
            )?,
            None,
            Cow::Borrowed(""),
        )),
    }
}

mod extended;
pub(crate) use extended::{
    eval_bit_count, eval_btrim, eval_get_bit, eval_get_byte, eval_regexp_count, eval_regexp_instr,
    eval_regexp_like, eval_regexp_substr, eval_set_bit, eval_set_byte, eval_sha224, eval_sha256,
    eval_sha384, eval_sha512, eval_unistr,
};

#[cfg(test)]
mod tests;

// =====================================================================
// Additional text function helpers
// =====================================================================

pub(super) fn eval_initcap(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "initcap")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            // ASCII fast path: walk bytes and case-fold via the
            // `make_ascii_*` byte ops, which are an order of magnitude
            // faster than `char::to_uppercase` / `to_lowercase` (each
            // returns an iterator that has to handle Unicode special
            // cases like German `ß` -> `SS`). Non-ASCII inputs keep
            // the chars-aware Unicode path so locale-sensitive
            // transformations stay correct.
            if s.is_ascii() {
                let mut result = Vec::with_capacity(s.len());
                let mut capitalize_next = true;
                for &b in s.as_bytes() {
                    if b.is_ascii_alphanumeric() {
                        if capitalize_next {
                            result.push(b.to_ascii_uppercase());
                            capitalize_next = false;
                        } else {
                            result.push(b.to_ascii_lowercase());
                        }
                    } else {
                        result.push(b);
                        capitalize_next = true;
                    }
                }
                return Ok(Value::Text(String::from_utf8(result).unwrap_or_default()));
            }
            let mut result = String::with_capacity(s.len());
            let mut capitalize_next = true;
            for ch in s.chars() {
                if ch.is_alphanumeric() {
                    if capitalize_next {
                        for upper in ch.to_uppercase() {
                            result.push(upper);
                        }
                        capitalize_next = false;
                    } else {
                        for lower in ch.to_lowercase() {
                            result.push(lower);
                        }
                    }
                } else {
                    result.push(ch);
                    capitalize_next = true;
                }
            }
            Ok(Value::Text(result))
        }
        _ => Err(DbError::internal("initcap() requires a text argument")),
    }
}

pub(super) fn eval_split_part(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "split_part")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "split_part()", "first")?;
    let delimiter = expect_text_arg(args, 1, "split_part()", "second")?;
    let field = expect_i32_value(&args[2], "split_part() third arg must be integer")?;
    if field == 0 {
        return Err(DbError::internal(
            "split_part(): field position must not be zero",
        ));
    }
    if field > 0 {
        let idx = nonneg_i32_to_usize(field.saturating_sub(1));
        Ok(Value::Text(
            s.split(delimiter).nth(idx).unwrap_or_default().to_string(),
        ))
    } else {
        // Negative field: count from the end (PG 14+)
        let idx = nonneg_i32_to_usize(field.saturating_neg().saturating_sub(1));
        Ok(Value::Text(
            s.rsplit(delimiter).nth(idx).unwrap_or_default().to_string(),
        ))
    }
}

pub(super) fn eval_composite_field(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "__aiondb_composite_field")?;
    let field_name = match &args[1] {
        Value::Null => return Ok(Value::Null),
        Value::Text(text) => text.as_str(),
        _ => {
            return Err(DbError::internal(
                "__aiondb_composite_field() field name must be text",
            ));
        }
    };

    if let Some(value) = super::eval_cypher_temporal_property_access(&args[0], field_name) {
        return Ok(value);
    }

    // JSONB property access: `node.prop` on a Cypher node/relationship
    // (which carries a JSONB props bag) should pull the named key out as
    // a JSONB scalar so downstream Cypher comparisons see the right type.
    if let Value::Jsonb(map) = &args[0] {
        if let Some(value) = map.as_object().and_then(|obj| obj.get(field_name)) {
            return Ok(jsonb_to_cypher_value(value));
        }
        return Ok(Value::Null);
    }

    let base = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Text(text) => text.as_str(),
        _ => return Ok(Value::Null),
    };

    let Some(field_index) = composite_field_index(field_name) else {
        return Ok(Value::Null);
    };
    let Some(fields) = parse_composite_text_fields(base) else {
        return Ok(Value::Null);
    };
    Ok(fields
        .get(field_index.saturating_sub(1))
        .cloned()
        .flatten()
        .map_or(Value::Null, Value::Text))
}

pub(super) fn eval_composite_assign(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "__aiondb_composite_assign")?;
    let field_name = match &args[1] {
        Value::Null => return Ok(Value::Null),
        Value::Text(text) => text.as_str(),
        _ => {
            return Err(DbError::internal(
                "__aiondb_composite_assign() field name must be text",
            ));
        }
    };
    let Some(field_index) = composite_field_index(field_name) else {
        return Ok(args[0].clone());
    };

    let mut fields = match &args[0] {
        Value::Null => Vec::new(),
        Value::Text(text) => parse_composite_text_fields(text).unwrap_or_default(),
        _ => Vec::new(),
    };
    let required_len = field_index.max(composite_field_min_arity(field_name));
    fields.resize(required_len, None);
    fields[field_index - 1] = composite_field_assignment_value(&args[2]);
    Ok(Value::Text(format_composite_text_fields(&fields)))
}

fn composite_field_index(field_name: &str) -> Option<usize> {
    let lower = field_name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "key" => Some(1),
        "value" => Some(2),
        "x" => Some(1),
        "y" => Some(2),
        _ => {
            let digits: String = lower
                .chars()
                .rev()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if digits.is_empty() {
                None
            } else {
                digits.parse::<usize>().ok().filter(|index| *index > 0)
            }
        }
    }
}

fn composite_field_min_arity(field_name: &str) -> usize {
    let lower = field_name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "key" | "value" | "x" | "y" => 2,
        _ => {
            let digits: String = lower
                .chars()
                .rev()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            match digits.as_str() {
                "1" | "2" => 2,
                _ => composite_field_index(field_name).unwrap_or(0),
            }
        }
    }
}

fn parse_composite_text_fields(text: &str) -> Option<Vec<Option<String>>> {
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    if inner.is_empty() {
        return Some(Vec::new());
    }

    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = inner.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '\\' => current.push(chars.next()?),
                '"' => {
                    if chars.peek().is_some_and(|next| *next == '"') {
                        let _ = chars.next();
                        current.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => current.push(other),
            }
            continue;
        }

        match ch {
            '"' => {
                quoted = true;
                in_quotes = true;
            }
            ',' => {
                fields.push(composite_field_value(&current, quoted));
                current.clear();
                quoted = false;
            }
            other => current.push(other),
        }
    }

    if in_quotes {
        return None;
    }
    fields.push(composite_field_value(&current, quoted));
    Some(fields)
}

fn composite_field_value(raw: &str, quoted: bool) -> Option<String> {
    if quoted || !raw.is_empty() {
        Some(raw.to_owned())
    } else {
        None
    }
}

fn composite_field_assignment_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.clone()),
        Value::Boolean(flag) => Some(if *flag { "t" } else { "f" }.to_owned()),
        other => Some(other.to_string()),
    }
}

fn format_composite_text_fields(fields: &[Option<String>]) -> String {
    let mut result = String::new();
    result.push('(');
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            result.push(',');
        }
        if let Some(value) = field {
            push_composite_text_value(&mut result, value);
        }
    }
    result.push(')');
    result
}

fn push_composite_text_value(result: &mut String, value: &str) {
    // Single-pass byte scan replaces 6 separate `String::contains` calls.
    // All 6 trigger bytes are ASCII; UTF-8 leading bytes (>= 0x80)
    // never collide. (Whitespace handling here is exactly ASCII space,
    // matching `value.contains(' ')`; no other whitespace
    // bytes are checked.)
    let needs_quote = value.is_empty()
        || value
            .as_bytes()
            .iter()
            .any(|b| matches!(*b, b',' | b'(' | b')' | b'"' | b'\\' | b' '));
    if !needs_quote {
        result.push_str(value);
        return;
    }
    result.push('"');
    // Bulk-copy chunks between escape triggers (`"`, `\\`) instead of
    // dispatching per char. Same shape as iter150's
    // `write_pg_array_scalar`.
    let bytes = value.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'"' && b != b'\\' {
            continue;
        }
        if idx > last {
            result.push_str(&value[last..idx]);
        }
        result.push('\\');
        result.push(b as char);
        last = idx + 1;
    }
    if last < bytes.len() {
        result.push_str(&value[last..]);
    }
    result.push('"');
}

pub(super) fn eval_translate(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "translate")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "translate()", "first")?;
    let from = expect_text_arg(args, 1, "translate()", "second")?;
    let to = expect_text_arg(args, 2, "translate()", "third")?;
    // ASCII fast path: build a 256-byte translation table where
    // every byte b is mapped to either Keep (no entry in `from`),
    // Delete (entry in `from` past `to`'s length), or Replace(b').
    // For an N-byte input the old code paid an O(M) linear scan
    // through `from_chars` per character; the table makes it O(1).
    if s.is_ascii() && from.is_ascii() && to.is_ascii() {
        // 0 = keep as-is (default); 1 = delete; otherwise the byte to emit + 2.
        // Encoding the action in a single u16 per slot avoids a parallel
        // bool array: table_lo[b] != 0 means there's an action, with
        // table_lo[b] == 1 -> delete; table_lo[b] >= 2 -> emit (table_lo[b] - 2).
        let mut table = [0u16; 256];
        let from_bytes = from.as_bytes();
        let to_bytes = to.as_bytes();
        for (i, &b) in from_bytes.iter().enumerate() {
            // First occurrence of `b` in `from` wins (matches PG semantics).
            if table[b as usize] != 0 {
                continue;
            }
            table[b as usize] = if i < to_bytes.len() {
                u16::from(to_bytes[i]) + 2
            } else {
                1
            };
        }
        let mut out = Vec::with_capacity(s.len());
        for &b in s.as_bytes() {
            match table[b as usize] {
                0 => out.push(b),
                1 => {} // delete
                v => out.push(u8::try_from(v - 2).unwrap_or(u8::MAX)),
            }
        }
        return Ok(Value::Text(String::from_utf8(out).unwrap_or_default()));
    }
    // Slow path: chars-aware for inputs that may contain multi-byte chars.
    let from_chars: Vec<char> = from.chars().collect();
    let to_chars: Vec<char> = to.chars().collect();
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if let Some(pos) = from_chars.iter().position(|&c| c == ch) {
            if pos < to_chars.len() {
                result.push(to_chars[pos]);
            }
            // else: char is deleted (to is shorter)
        } else {
            result.push(ch);
        }
    }
    Ok(Value::Text(result))
}

pub(super) fn eval_overlay(args: &[Value]) -> DbResult<Value> {
    if args.len() < 3 || args.len() > 4 {
        return Err(DbError::internal("overlay() requires 3 or 4 arguments"));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s_buf;
    let s = match &args[0] {
        Value::Text(s) => s.as_str(),
        other => {
            s_buf = other.to_string();
            s_buf.as_str()
        }
    };
    let r_buf;
    let replacement = match &args[1] {
        Value::Text(r) => r.as_str(),
        other => {
            r_buf = other.to_string();
            r_buf.as_str()
        }
    };
    let start = expect_i32_value(&args[2], "overlay() third arg must be integer")?;
    let start_idx = nonneg_i32_to_usize(start.saturating_sub(1).max(0));
    let count_arg = if args.len() == 4 {
        let c = expect_i32_value(&args[3], "overlay() fourth arg must be integer")?;
        if c < 0 {
            return Err(DbError::internal("negative substring length not allowed"));
        }
        Some(c)
    } else {
        None
    };
    // ASCII fast path: char positions equal byte positions, so we can
    // splice via byte slices without ever materialising a `Vec<char>`
    // for the input. For the old chars-based path, the prefix walk
    // and suffix walk both ran in lockstep with the input length.
    if s.is_ascii() {
        let s_bytes = s.as_bytes();
        let total = s_bytes.len();
        let count = match count_arg {
            Some(c) => nonneg_i32_to_usize(c.max(0)),
            None => {
                if replacement.is_ascii() {
                    replacement.len()
                } else {
                    replacement.chars().count()
                }
            }
        };
        let prefix_end = start_idx.min(total);
        let suffix_start = start_idx.saturating_add(count).min(total);
        let mut out =
            String::with_capacity(prefix_end + replacement.len() + (total - suffix_start));
        out.push_str(&s[..prefix_end]);
        out.push_str(replacement);
        out.push_str(&s[suffix_start..]);
        return Ok(Value::Text(out));
    }
    let count = match count_arg {
        Some(c) => nonneg_i32_to_usize(c.max(0)),
        None => replacement.chars().count(),
    };
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len());
    // Before the overlay
    for &ch in chars.iter().take(start_idx) {
        result.push(ch);
    }
    // The replacement
    result.push_str(replacement);
    // After the overlay
    let skip = start_idx + count;
    for &ch in chars.iter().skip(skip) {
        result.push(ch);
    }
    Ok(Value::Text(result))
}

pub(super) fn eval_bit_length(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "bit_length")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::Int(to_i32_saturating(s.len().saturating_mul(8)))),
        _ => Err(DbError::internal("bit_length() requires a text argument")),
    }
}

pub(super) fn eval_chr(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "chr")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Int(n) => {
            let code = *n;
            if code == 0 {
                return Err(DbError::internal("chr(): null character not permitted"));
            }
            let ch = u32::try_from(code)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    DbError::internal(format!("chr(): invalid character code {code}"))
                })?;
            Ok(Value::Text(ch.to_string()))
        }
        Value::BigInt(n) => {
            let code = *n;
            if code == 0 {
                return Err(DbError::internal("chr(): null character not permitted"));
            }
            let ch = u32::try_from(code)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    DbError::internal(format!("chr(): invalid character code {code}"))
                })?;
            Ok(Value::Text(ch.to_string()))
        }
        _ => Err(DbError::internal("chr() requires an integer argument")),
    }
}

pub(super) fn eval_ascii(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "ascii")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            let code = s
                .chars()
                .next()
                .map_or(0, |ch| i32::try_from(u32::from(ch)).unwrap_or(i32::MAX));
            Ok(Value::Int(code))
        }
        _ => Err(DbError::internal("ascii() requires a text argument")),
    }
}

pub(super) fn eval_md5(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "md5")?;
    let digest = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Text(s) => Md5::digest(s.as_bytes()),
        Value::Blob(b) => Md5::digest(b),
        // Coerce to text for PG compatibility.
        other => Md5::digest(other.to_string().as_bytes()),
    };
    Ok(Value::Text(aiondb_core::hex_encode(&digest)))
}

pub(super) fn eval_quote_literal(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "quote_literal")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        value => {
            let rendered = super::value_to_text(value);
            // PG's `text` rejects NUL bytes; mirror that here so a NUL-truncated
            // downstream consumer (printf %s in a log line, FFI to libpq) cannot
            // see a literal that ends earlier than the engine recorded
            // (audit errfmt F3).
            if rendered.contains('\0') {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "quote_literal() does not accept NUL in the input",
                ));
            }
            let use_escape_syntax = rendered.contains('\\');
            let mut escaped = String::with_capacity(rendered.len() + 2);
            for ch in rendered.chars() {
                match ch {
                    '\'' => escaped.push_str("''"),
                    '\\' if use_escape_syntax => escaped.push_str("\\\\"),
                    other => escaped.push(other),
                }
            }
            if use_escape_syntax {
                Ok(Value::Text(format!("E'{escaped}'")))
            } else {
                Ok(Value::Text(format!("'{escaped}'")))
            }
        }
    }
}

pub(super) fn eval_quote_ident(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "quote_ident")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            // PG identifiers do not allow `\0` and treat CR/LF as whitespace.
            // Reflecting them through `quote_ident` re-emits log lines with
            // smuggled newlines and lets a NUL-truncated downstream consumer
            // see a different identifier than the engine recorded
            // (audit errfmt F2).
            if s.chars().any(|c| c == '\0' || c == '\r' || c == '\n') {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "quote_ident() does not accept NUL/CR/LF in the identifier",
                ));
            }
            // PostgreSQL: only quote if the identifier needs quoting
            let needs_quoting = s.is_empty()
                || s.starts_with(|c: char| c.is_ascii_digit())
                || s.chars()
                    .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
            if needs_quoting {
                let escaped = s.replace('"', "\"\"");
                Ok(Value::Text(format!("\"{escaped}\"")))
            } else {
                Ok(Value::Text(s.clone()))
            }
        }
        _ => Err(DbError::internal("quote_ident() requires a text argument")),
    }
}

pub(super) fn eval_quote_nullable(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "quote_nullable")?;
    fn check_nul(s: &str) -> DbResult<()> {
        if s.contains('\0') {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                "quote_nullable() does not accept NUL in the input",
            ));
        }
        Ok(())
    }
    fn render(input: &str) -> String {
        // Mirror `quote_literal`: when the text contains a backslash, switch
        // to PostgreSQL escape-string syntax (`E'...'`) and double both `'`
        // and `\\`. Without this, `quote_nullable` would emit `'a\'' OR …'`
        // — a closing single quote followed by attacker-controlled bytes
        // (audit errfmt F1).
        let use_escape_syntax = input.contains('\\');
        let mut escaped = String::with_capacity(input.len() + 2);
        for ch in input.chars() {
            match ch {
                '\'' => escaped.push_str("''"),
                '\\' if use_escape_syntax => escaped.push_str("\\\\"),
                other => escaped.push(other),
            }
        }
        if use_escape_syntax {
            format!("E'{escaped}'")
        } else {
            format!("'{escaped}'")
        }
    }
    match &args[0] {
        Value::Null => Ok(Value::Text("NULL".to_string())),
        Value::Text(s) => {
            check_nul(s)?;
            Ok(Value::Text(render(s)))
        }
        other => {
            let r = other.to_string();
            check_nul(&r)?;
            Ok(Value::Text(render(&r)))
        }
    }
}

pub(super) fn eval_to_hex(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_hex")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Int(n) => Ok(Value::Text(format!("{:x}", *n))),
        Value::BigInt(n) => Ok(Value::Text(format!("{:x}", *n))),
        _ => Err(DbError::internal("to_hex() requires an integer argument")),
    }
}

pub(super) fn eval_regexp_replace(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 3, 6, "regexp_replace() requires 3 to 6 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = expect_text_arg(args, 0, "regexp_replace()", "first")?;
    let pattern = expect_text_arg(args, 1, "regexp_replace()", "second")?;
    let replacement = expect_text_arg(args, 2, "regexp_replace()", "third")?.to_owned();
    // PostgreSQL signatures:
    //   regexp_replace(source, pattern, replacement [, flags])          -- 3-4 args (4th is text)
    //   regexp_replace(source, pattern, replacement, start [, N [, flags]]) -- 4-6 args (4th is int)
    let (start_pos, replace_n, flags): (i32, Option<i32>, Cow<'_, str>) = if args.len() == 4 {
        parse_regexp_replace_fourth_arg(&args[3])?
    } else if args.len() >= 5 {
        let start = expect_i32_value(
            &args[3],
            "regexp_replace() fourth arg (start) must be integer",
        )?;
        let n = expect_i32_value(&args[4], "regexp_replace() fifth arg (N) must be integer")?;
        let f = if args.len() == 6 {
            match &args[5] {
                Value::Text(s) => Cow::Borrowed(s.as_str()),
                _ => {
                    return Err(DbError::internal(
                        "regexp_replace() sixth arg (flags) must be text",
                    ));
                }
            }
        } else {
            Cow::Borrowed("")
        };
        (start, Some(n), f)
    } else {
        (1, None, Cow::Borrowed(""))
    };

    // Validate start
    if start_pos <= 0 {
        return Err(DbError::internal(format!(
            "invalid value for parameter \"start\": {start_pos}"
        )));
    }
    // Validate N
    if let Some(n) = replace_n {
        if n < 0 {
            return Err(DbError::internal(format!(
                "invalid value for parameter \"n\": {n}"
            )));
        }
    }
    // Validate flags
    for ch in flags.chars() {
        if !matches!(
            ch,
            'g' | 'i' | 'c' | 'e' | 'n' | 'm' | 'p' | 'q' | 's' | 'w' | 'x'
        ) {
            let mut err = DbError::internal(format!("invalid regular expression option: \"{ch}\""));
            // When the 4th arg was text (flags) and looks like a digit,
            // PG adds a hint suggesting to cast to integer.
            if replace_n.is_none() && ch.is_ascii_digit() {
                err = err.with_client_hint(
                    "If you meant to use regexp_replace() with a start parameter, cast the fourth argument to integer explicitly."
                );
            }
            return Err(err);
        }
    }

    let case_insensitive = flags.contains('i');
    let global = flags.contains('g');

    // Guard against regex DoS from excessively large patterns.
    const MAX_REGEX_PATTERN_LEN: usize = 32_768;
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Ok(Value::Text(source.to_owned()));
    }

    // Pull from the per-thread regex cache so repeated regexp_replace calls
    // over a scan compile the pattern once instead of once per row.
    let Ok(re) = crate::regex_cache::get_ci(pattern, case_insensitive) else {
        return Ok(Value::Text(source.to_owned()));
    };

    let rust_replacement = pg_to_rust_replacement(&replacement);

    // In the extended form (5+ args), N=0 means "replace all" and 'g' flag is
    // ignored when N>0. When N is not provided (None), 'g' flag applies normally.
    let replace_all = match replace_n {
        Some(0) => true,  // N=0 explicitly means replace all
        Some(_) => false, // N>0: replace only that occurrence ('g' ignored)
        None => global,   // No N: use 'g' flag
    };

    let byte_start = char_to_byte_pos(source, nonneg_i32_to_usize(start_pos.saturating_sub(1)));
    let prefix = &source[..byte_start];
    let search_str = &source[byte_start..];

    if let Some(n) = replace_n {
        if n > 0 {
            // Replace only the N-th occurrence within the search portion
            let mut match_count = 0usize;
            let mut result = String::from(prefix);
            let mut last_end = 0;
            let mut replaced = false;
            for caps in re.captures_iter(search_str) {
                match_count += 1;
                let Some(m) = caps.get(0) else {
                    continue;
                };
                if match_count == nonneg_i32_to_usize(n) {
                    result.push_str(&search_str[last_end..m.start()]);
                    caps.expand(&rust_replacement, &mut result);
                    last_end = m.end();
                    replaced = true;
                    break;
                }
            }
            if replaced {
                result.push_str(&search_str[last_end..]);
            } else {
                result.push_str(search_str);
            }
            return Ok(Value::Text(result));
        }
    }

    if replace_all {
        let replaced = re
            .replace_all(search_str, rust_replacement.as_str())
            .into_owned();
        Ok(Value::Text(format!("{prefix}{replaced}")))
    } else {
        let replaced = re
            .replace(search_str, rust_replacement.as_str())
            .into_owned();
        Ok(Value::Text(format!("{prefix}{replaced}")))
    }
}

/// Convert a 0-based character offset to byte offset in UTF-8.
fn char_to_byte_pos(s: &str, char_offset: usize) -> usize {
    s.char_indices()
        .nth(char_offset)
        .map_or(s.len(), |(bp, _)| bp)
}

/// Convert `PostgreSQL` replacement backreferences (\1, \2, \&, \\) to the
/// Rust `regex` crate format ($1, $2, $0, \).
fn pg_to_rust_replacement(replacement: &str) -> String {
    // Walk via `char_indices` so we never materialise a `Vec<char>`
    // for the input. The escape rules only inspect ASCII characters
    // (`\\`, ASCII digit, `&`, `$`), so we can peek the next char
    // by jumping a single iterator step rather than indexing.
    let mut result = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some(next) if next.is_ascii_digit() => {
                    // \1..\9 → $1..$9
                    chars.next();
                    result.push('$');
                    result.push(next);
                }
                Some('&') => {
                    // \& → $0 (whole match)
                    chars.next();
                    result.push_str("$0");
                }
                Some('\\') => {
                    // \\ → literal backslash
                    chars.next();
                    result.push('\\');
                }
                Some(other) => {
                    // Unrecognized escape: pass through as-is (PG behaviour)
                    chars.next();
                    result.push('\\');
                    result.push(other);
                }
                None => {
                    // Trailing backslash: keep it as-is.
                    result.push('\\');
                }
            }
        } else if ch == '$' {
            // Escape literal `$` so the regex crate does not treat it
            // as a capture reference.
            result.push_str("$$");
        } else {
            result.push(ch);
        }
    }
    result
}

/// Build a regex with PostgreSQL-compatible flags.
///
/// Returns `(Arc<Regex>, bool)` where the bool indicates whether the 'g'
/// flag is present. The compiled regex is sourced from the per-thread cache
/// to amortise compile cost across long scans.
fn build_pg_regex(pattern: &str, flags: &str) -> Result<(std::sync::Arc<Regex>, bool), DbError> {
    // Guard against regex DoS from excessively large patterns that can
    // cause the regex compiler to use excessive CPU/memory.
    const MAX_REGEX_PATTERN_LEN: usize = 32_768;
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(DbError::internal(format!(
            "regular expression pattern too large ({} bytes, max {})",
            pattern.len(),
            MAX_REGEX_PATTERN_LEN
        )));
    }

    let case_insensitive = flags.contains('i');
    let global = flags.contains('g');

    let re = crate::regex_cache::get_ci(pattern, case_insensitive)
        .map_err(|e| DbError::internal(format!("invalid regular expression: {e}")))?;
    Ok((re, global))
}

/// Extract capture groups from a regex match as a `Value::Array` of text elements.
///
/// If capture groups are present, returns one element per group.
/// If no capture groups, returns the full match as a single-element array.
fn captures_to_array(caps: &regex::Captures<'_>) -> Value {
    let num_groups = caps.len(); // includes group 0
    if num_groups > 1 {
        // Has explicit capture groups - return groups 1..N
        let elements: Vec<Value> = (1..num_groups)
            .map(|i| match caps.get(i) {
                Some(m) => Value::Text(m.as_str().to_string()),
                None => Value::Null,
            })
            .collect();
        Value::Array(elements)
    } else {
        // No capture groups - return full match as single-element array
        Value::Array(vec![Value::Text(caps[0].to_string())])
    }
}

/// `regexp_match(source, pattern [, flags])` - returns `text[]` (first match only).
///
/// PostgreSQL: returns NULL if no match, otherwise an array of captured substrings.
pub(super) fn eval_regexp_match(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "regexp_match() requires 2 or 3 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = expect_text_arg(args, 0, "regexp_match()", "first")?;
    let pattern = expect_text_arg(args, 1, "regexp_match()", "second")?;
    let flags = if args.len() == 3 {
        expect_text_arg(args, 2, "regexp_match()", "third")?
    } else {
        ""
    };
    // The 'g' flag is not valid for regexp_match (PG raises an error)
    if flags.contains('g') {
        return Err(DbError::internal(
            "regexp_match() does not support the \"global\" option",
        ));
    }
    let Ok((re, _global)) = build_pg_regex(pattern, flags) else {
        return Ok(Value::Null);
    };
    match re.captures(source) {
        Some(caps) => Ok(captures_to_array(&caps)),
        None => Ok(Value::Null),
    }
}

/// `regexp_matches(source, pattern [, flags])` - returns `setof text[]`.
///
/// Without 'g' flag: returns one row (like `regexp_match`).
/// With 'g' flag: returns one row per match (set-returning function).
///
/// We return `Value::Array(vec![...])` where each element is itself a
/// `Value::Array(text[])` row. The SRF expansion handles unpacking.
pub(super) fn eval_regexp_matches(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "regexp_matches() requires 2 or 3 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = expect_text_arg(args, 0, "regexp_matches()", "first")?;
    let pattern = expect_text_arg(args, 1, "regexp_matches()", "second")?;
    let flags = if args.len() == 3 {
        expect_text_arg(args, 2, "regexp_matches()", "third")?
    } else {
        ""
    };
    let Ok((re, global)) = build_pg_regex(pattern, flags) else {
        return Ok(Value::Array(Vec::new()));
    };
    if global {
        // Return all matches
        let mut rows: Vec<Value> = Vec::new();
        for caps in re.captures_iter(source) {
            rows.push(captures_to_array(&caps));
        }
        Ok(Value::Array(rows))
    } else {
        // Single match - but still return as a single-element set
        match re.captures(source) {
            Some(caps) => Ok(Value::Array(vec![captures_to_array(&caps)])),
            None => Ok(Value::Array(Vec::new())),
        }
    }
}

/// `regexp_split_to_array(source, pattern [, flags])` - returns `text[]`.
pub(super) fn eval_regexp_split_to_array(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(
        args,
        2,
        3,
        "regexp_split_to_array() requires 2 or 3 arguments",
    )?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let source = expect_text_arg(args, 0, "regexp_split_to_array()", "first")?;
    let pattern = expect_text_arg(args, 1, "regexp_split_to_array()", "second")?;
    let flags = if args.len() == 3 {
        expect_text_arg(args, 2, "regexp_split_to_array()", "third")?
    } else {
        ""
    };
    // 'g' flag is ignored for split functions (they always split globally)
    let Ok((re, _global)) = build_pg_regex(pattern, flags) else {
        // Invalid pattern: return the source as a single-element array
        return Ok(Value::Array(vec![Value::Text(source.to_string())]));
    };
    let parts: Vec<Value> = re
        .split(source)
        .map(|part| Value::Text(part.to_string()))
        .collect();
    Ok(Value::Array(parts))
}

/// `regexp_split_to_table(source, pattern [, flags])` - returns `setof text`.
///
/// Like `regexp_split_to_array`, but returns a set of rows.
/// We return `Value::Array(...)` and the SRF expansion will handle unpacking.
pub(super) fn eval_regexp_split_to_table(args: &[Value]) -> DbResult<Value> {
    // Implementation is identical to regexp_split_to_array;
    // the SRF expansion mechanism treats the result array as rows.
    eval_regexp_split_to_array(args)
}

pub(super) fn eval_encode(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "encode")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let data = match &args[0] {
        Value::Blob(b) => b.as_slice(),
        Value::Text(s) => s.as_bytes(),
        _ => {
            return Err(DbError::internal(
                "encode() first arg must be bytea or text",
            ));
        }
    };
    let format = expect_text_arg(args, 1, "encode()", "second")?.to_lowercase();
    match format.as_str() {
        "hex" => Ok(Value::Text(aiondb_core::hex_encode(data))),
        "base64" => Ok(Value::Text(BASE64.encode(data))),
        "escape" => {
            // PG escape format: zero bytes and high-bit bytes (>= 0x80) become
            // octal `\NNN`; backslash is doubled; every other byte (incl.
            // controls 0x01..0x1F and 0x7F) passes through as its raw byte.
            let mut out = String::with_capacity(data.len());
            for &b in data {
                if b == b'\\' {
                    out.push_str("\\\\");
                } else if b == 0 || b >= 0x80 {
                    out.push('\\');
                    out.push(char::from(b'0' + (b >> 6)));
                    out.push(char::from(b'0' + ((b >> 3) & 0o7)));
                    out.push(char::from(b'0' + (b & 0o7)));
                } else {
                    out.push(b as char);
                }
            }
            Ok(Value::Text(out))
        }
        _ => Err(DbError::internal(format!(
            "encode(): unrecognized format '{format}'"
        ))),
    }
}

pub(super) fn eval_decode(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "decode")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let input = expect_text_arg(args, 0, "decode()", "first")?;
    let format = expect_text_arg(args, 1, "decode()", "second")?.to_lowercase();
    match format.as_str() {
        "hex" => {
            let bytes = hex_decode(input)
                .map_err(|e| DbError::internal(format!("decode(): invalid hex string: {e}")))?;
            Ok(Value::Blob(bytes))
        }
        "base64" => {
            let bytes = BASE64
                .decode(input)
                .map_err(|e| DbError::internal(format!("decode(): invalid base64 string: {e}")))?;
            Ok(Value::Blob(bytes))
        }
        "escape" => {
            // PG escape format: `\NNN` octal (digits 0-7 only) or `\\` for
            // a literal backslash. The previous implementation accepted any
            // ASCII digit in the octal triplet and computed
            // `(d - b'0') * 64 + ...` in `u8`, which panics in debug on any
            // triplet containing a 4..9 digit (`9 * 64 = 576 > u8::MAX`) and
            // the arithmetic in `u16` before narrowing.
            let input_bytes = input.as_bytes();
            let mut out = Vec::with_capacity(input_bytes.len());
            let mut i = 0;
            while i < input_bytes.len() {
                if input_bytes[i] == b'\\' && i + 1 < input_bytes.len() {
                    if input_bytes[i + 1] == b'\\' {
                        out.push(b'\\');
                        i += 2;
                    } else if i + 3 < input_bytes.len()
                        && (b'0'..=b'7').contains(&input_bytes[i + 1])
                        && (b'0'..=b'7').contains(&input_bytes[i + 2])
                        && (b'0'..=b'7').contains(&input_bytes[i + 3])
                    {
                        let d1 = u16::from(input_bytes[i + 1] - b'0');
                        let d2 = u16::from(input_bytes[i + 2] - b'0');
                        let d3 = u16::from(input_bytes[i + 3] - b'0');
                        let val = d1 * 64 + d2 * 8 + d3;
                        // 7*64 + 7*8 + 7 = 511 > 255, so reject values that
                        // do not fit in a byte (PG only allows 0..=255).
                        if val > u16::from(u8::MAX) {
                            return Err(DbError::internal(format!(
                                "decode(): octal escape {val} out of byte range"
                            )));
                        }
                        out.push(u8::try_from(val).unwrap_or(u8::MAX));
                        i += 4;
                    } else {
                        out.push(input_bytes[i]);
                        i += 1;
                    }
                } else {
                    out.push(input_bytes[i]);
                    i += 1;
                }
            }
            Ok(Value::Blob(out))
        }
        _ => Err(DbError::internal(format!(
            "decode(): unrecognized format '{format}'"
        ))),
    }
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // Operate on &[u8] throughout: slicing a `&str` by byte index panics
    // when the slice does not fall on a UTF-8 char boundary, which a
    // caller-controlled input like `decode('한X', 'hex')` can trigger
    // via the SQL `decode(..., 'hex')` function.
    let bytes_in = s.trim().as_bytes();
    if !bytes_in.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_string());
    }
    let mut bytes = Vec::with_capacity(bytes_in.len() / 2);
    for (i, pair) in bytes_in.chunks_exact(2).enumerate() {
        let hi = hex_nibble(pair[0]).ok_or_else(|| format!("invalid hex at position {}", i * 2))?;
        let lo =
            hex_nibble(pair[1]).ok_or_else(|| format!("invalid hex at position {}", i * 2 + 1))?;
        bytes.push((hi << 4) | lo);
    }
    Ok(bytes)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
