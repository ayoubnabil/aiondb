use super::*;

// =====================================================================
// PG array literal parser: "{1,2,3}" -> Value::Array(...)
// =====================================================================

/// Parse a PG array literal string into `Value::Array`.
/// Handles multi-dimensional arrays, quoted strings, NULL elements, and
/// type-aware element parsing.
pub(super) fn parse_pg_array_text(s: &str, elem_type: &DataType) -> DbResult<Value> {
    let s = s.trim();
    let s = if let Some(eq_pos) = s.find('=') {
        let prefix = &s[..eq_pos];
        if prefix.starts_with('[') {
            &s[eq_pos + 1..]
        } else {
            s
        }
    } else {
        s
    };

    if !s.starts_with('{') || !s.ends_with('}') {
        return Err(DbError::invalid_input_syntax("array", s));
    }

    validate_pg_array_literal(s)?;
    parse_pg_array_elements(s, elem_type)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayLiteralElementKind {
    Scalar,
    Array,
}

fn validate_pg_array_literal(s: &str) -> DbResult<()> {
    let bytes = s.as_bytes();
    let mut pos = 0;
    validate_pg_array_level(bytes, &mut pos, 0, s)?;
    skip_array_whitespace(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(malformed_array_literal_error(
            s,
            "Junk after closing right brace.",
        ));
    }
    Ok(())
}

fn validate_pg_array_level(
    bytes: &[u8],
    pos: &mut usize,
    depth: usize,
    literal: &str,
) -> DbResult<ArrayLiteralElementKind> {
    // Mirror `CAST_ARRAY_MAX_DEPTH` at the parser level so a malicious
    // `{{{...}}}` literal cannot drive a stack overflow before the cast layer
    // gets to enforce its own cap.
    const MAX_PG_ARRAY_LITERAL_DEPTH: usize = 256;
    if depth > MAX_PG_ARRAY_LITERAL_DEPTH {
        return Err(malformed_array_literal_error(
            literal,
            "Array nesting depth exceeds 256.",
        ));
    }
    debug_assert_eq!(bytes.get(*pos), Some(&b'{'));
    *pos += 1;
    skip_array_whitespace(bytes, pos);

    if *pos >= bytes.len() {
        return Err(malformed_array_literal_error(
            literal,
            "Unexpected end of input.",
        ));
    }

    if bytes[*pos] == b'}' {
        *pos += 1;
        if depth > 0 {
            return Err(malformed_array_literal_error(
                literal,
                "Unexpected \"}\" character.",
            ));
        }
        return Ok(ArrayLiteralElementKind::Scalar);
    }

    let mut expected_kind = None;
    loop {
        skip_array_whitespace(bytes, pos);
        if *pos >= bytes.len() {
            return Err(malformed_array_literal_error(
                literal,
                "Unexpected end of input.",
            ));
        }

        let kind = match bytes[*pos] {
            b'{' => {
                if expected_kind == Some(ArrayLiteralElementKind::Scalar) {
                    return Err(malformed_array_literal_error(
                        literal,
                        "Unexpected \"{\" character.",
                    ));
                }
                validate_pg_array_level(bytes, pos, depth + 1, literal)?;
                ArrayLiteralElementKind::Array
            }
            b'}' => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected \"}\" character.",
                ));
            }
            b'\\' => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected \"\\\" character.",
                ));
            }
            b'"' => {
                validate_pg_array_quoted_element(bytes, pos, literal)?;
                ArrayLiteralElementKind::Scalar
            }
            _ => {
                if expected_kind == Some(ArrayLiteralElementKind::Array) {
                    return Err(malformed_array_literal_error(
                        literal,
                        "Unexpected array element.",
                    ));
                }
                validate_pg_array_unquoted_element(bytes, pos, literal)?;
                ArrayLiteralElementKind::Scalar
            }
        };

        if expected_kind.is_none() {
            expected_kind = Some(kind);
        }

        skip_array_whitespace(bytes, pos);
        if *pos >= bytes.len() {
            return Err(malformed_array_literal_error(
                literal,
                "Unexpected end of input.",
            ));
        }

        match bytes[*pos] {
            b',' => *pos += 1,
            b'}' => {
                *pos += 1;
                return Ok(expected_kind.unwrap_or(ArrayLiteralElementKind::Scalar));
            }
            _ => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected array element.",
                ));
            }
        }
    }
}

fn validate_pg_array_quoted_element(bytes: &[u8], pos: &mut usize, literal: &str) -> DbResult<()> {
    debug_assert_eq!(bytes.get(*pos), Some(&b'"'));
    *pos += 1;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\\' => {
                if *pos + 1 >= bytes.len() {
                    return Err(malformed_array_literal_error(
                        literal,
                        "Unexpected \"\\\" character.",
                    ));
                }
                *pos += 2;
            }
            b'"' => {
                *pos += 1;
                return Ok(());
            }
            _ => *pos += 1,
        }
    }

    Err(malformed_array_literal_error(
        literal,
        "Unexpected end of input.",
    ))
}

fn validate_pg_array_unquoted_element(
    bytes: &[u8],
    pos: &mut usize,
    literal: &str,
) -> DbResult<()> {
    let start = *pos;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b',' | b'}' => break,
            b'{' => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected \"{\" character.",
                ));
            }
            b'\\' => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected \"\\\" character.",
                ));
            }
            b'"' => {
                return Err(malformed_array_literal_error(
                    literal,
                    "Unexpected array element.",
                ));
            }
            _ => *pos += 1,
        }
    }

    if bytes[start..*pos]
        .iter()
        .all(|byte| byte.is_ascii_whitespace())
    {
        return Err(malformed_array_literal_error(
            literal,
            "Unexpected array element.",
        ));
    }

    Ok(())
}

fn skip_array_whitespace(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
}

fn malformed_array_literal_error(literal: &str, detail: &str) -> DbError {
    DbError::from_report(
        ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            format!("malformed array literal: \"{literal}\""),
        )
        .with_client_detail(detail),
    )
}

pub(super) fn has_explicit_array_bounds(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('[') && s.contains('=')
}

fn parse_pg_array_elements(s: &str, elem_type: &DataType) -> DbResult<Value> {
    debug_assert!(s.starts_with('{') && s.ends_with('}'));
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let trimmed = inner.trim_start();
    if trimmed.starts_with('{') {
        let sub_arrays = split_top_level_elements(inner);
        let mut result = Vec::with_capacity(sub_arrays.len());
        for sub in sub_arrays {
            let sub = sub.trim();
            if sub.starts_with('{') && sub.ends_with('}') {
                result.push(parse_pg_array_elements(sub, elem_type)?);
            } else {
                return Err(DbError::invalid_input_syntax("array", s));
            }
        }
        return Ok(Value::Array(result));
    }

    let raw_elems = split_top_level_elements(inner);
    let mut result = Vec::with_capacity(raw_elems.len());
    for raw in raw_elems {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("NULL") {
            result.push(Value::Null);
        } else {
            let unquoted = unquote_array_element(raw);
            let val = parse_array_element(&unquoted, elem_type)?;
            result.push(val);
        }
    }
    Ok(Value::Array(result))
}

fn split_top_level_elements(s: &str) -> Vec<&str> {
    let mut elements = Vec::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_quote {
            if ch == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if ch == b'"' {
                in_quote = false;
            }
        } else {
            match ch {
                b'"' => in_quote = true,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b',' if depth == 0 => {
                    elements.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start <= s.len() {
        elements.push(&s[start..]);
    }
    elements
}

fn unquote_array_element(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        s.to_owned()
    }
}

fn parse_array_element(s: &str, elem_type: &DataType) -> DbResult<Value> {
    match elem_type {
        DataType::Int => s
            .trim()
            .parse::<i32>()
            .map(Value::Int)
            .map_err(|_| DbError::invalid_input_syntax("integer", s)),
        DataType::BigInt => s
            .trim()
            .parse::<i64>()
            .map(Value::BigInt)
            .map_err(|_| DbError::invalid_input_syntax("bigint", s)),
        DataType::Real => s
            .trim()
            .parse::<f32>()
            .map(Value::Real)
            .map_err(|_| DbError::invalid_input_syntax("real", s)),
        DataType::Double => s
            .trim()
            .parse::<f64>()
            .map(Value::Double)
            .map_err(|_| DbError::invalid_input_syntax("double precision", s)),
        DataType::Numeric => s
            .trim()
            .parse::<NumericValue>()
            .map(Value::Numeric)
            .map_err(|_| DbError::invalid_input_syntax("numeric", s)),
        DataType::Boolean => {
            // Skip the unconditional `to_ascii_lowercase()` allocation  -
            // dominant array-literal boolean tokens (`t`, `f`, `true`,
            // `false`) are typed in canonical lowercase, and the
            // `eq_ignore_ascii_case` byte-pair compare short-circuits
            // on length/byte mismatch.
            let trimmed = s.trim();
            if trimmed.eq_ignore_ascii_case("t")
                || trimmed.eq_ignore_ascii_case("true")
                || trimmed.eq_ignore_ascii_case("yes")
                || trimmed.eq_ignore_ascii_case("on")
                || trimmed == "1"
            {
                Ok(Value::Boolean(true))
            } else if trimmed.eq_ignore_ascii_case("f")
                || trimmed.eq_ignore_ascii_case("false")
                || trimmed.eq_ignore_ascii_case("no")
                || trimmed.eq_ignore_ascii_case("off")
                || trimmed == "0"
            {
                Ok(Value::Boolean(false))
            } else {
                Err(DbError::invalid_input_syntax("boolean", s))
            }
        }
        DataType::Text => Ok(Value::Text(s.to_owned())),
        _ => cast_value(Value::Text(s.to_owned()), elem_type),
    }
}
