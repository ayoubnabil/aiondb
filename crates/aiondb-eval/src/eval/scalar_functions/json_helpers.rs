use super::*;
use crate::eval::scalar_functions::value_convert::{f64_to_i64, i64_to_f64};
use std::borrow::Cow;
use std::collections::HashSet;

fn normalized_json_index(len: usize, idx: i64) -> Option<usize> {
    if idx >= 0 {
        return usize::try_from(idx).ok();
    }
    let len_i64 = i64::try_from(len).ok()?;
    let adjusted = len_i64.checked_add(idx)?;
    if adjusted < 0 {
        None
    } else {
        usize::try_from(adjusted).ok()
    }
}

fn normalized_json_insert_index(len: usize, idx: i64, insert_after: bool) -> Option<usize> {
    let len_i64 = i64::try_from(len).ok()?;
    let mut adjusted = if idx < 0 {
        len_i64.checked_add(idx)?.checked_add(1)?
    } else {
        idx
    };
    if insert_after {
        adjusted = adjusted.saturating_add(1);
    }
    let clamped = adjusted.clamp(0, len_i64);
    usize::try_from(clamped).ok()
}

pub(super) fn eval_json_object(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Jsonb(serde_json::Value::Object(
            serde_json::Map::new(),
        )));
    }
    if !args.len().is_multiple_of(2) {
        return Err(DbError::internal(
            "JSON_OBJECT requires an even number of arguments (key/value pairs)".to_string(),
        ));
    }
    let mut map = serde_json::Map::new();
    for pair in args.chunks(2) {
        let key = match &pair[0] {
            Value::Text(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        let val = value_to_json(&pair[1]);
        map.insert(key, val);
    }
    Ok(Value::Jsonb(serde_json::Value::Object(map)))
}

pub(super) fn eval_json_array(args: &[Value]) -> Value {
    let arr: Vec<serde_json::Value> = args.iter().map(value_to_json).collect();
    Value::Jsonb(serde_json::Value::Array(arr))
}

pub(super) fn eval_json_array_subquery(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_json_array_subquery")?;
    // Iterate the source slice in place rather than cloning the entire array
    // before filtering; the downstream `value_to_json` mapping only borrows.
    let arr: Vec<serde_json::Value> = match &args[0] {
        Value::Null => Vec::new(),
        Value::Array(values) => values
            .iter()
            .filter(|value: &&Value| !value.is_null())
            .map(value_to_json)
            .collect(),
        _ => {
            return Err(DbError::internal(
                "__aiondb_json_array_subquery() expects an array argument",
            ));
        }
    };
    Ok(Value::Jsonb(serde_json::Value::Array(arr)))
}

pub(super) fn eval_json_scalar(args: &[Value]) -> Value {
    if args.is_empty() {
        return Value::Jsonb(serde_json::Value::Null);
    }
    Value::Jsonb(value_to_json(&args[0]))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonPredicateKind {
    Any,
    Object,
    Array,
    Scalar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonUniqueMode {
    Default,
    With,
    Without,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonTopKind {
    Object,
    Array,
    Scalar,
}

struct JsonScanner<'a> {
    bytes: &'a [u8],
    index: usize,
    saw_duplicate_keys: bool,
}

impl<'a> JsonScanner<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            index: 0,
            saw_duplicate_keys: false,
        }
    }

    fn parse(mut self) -> Option<(JsonTopKind, bool)> {
        self.skip_ws();
        let top = self.parse_value()?;
        self.skip_ws();
        if self.index != self.bytes.len() {
            return None;
        }
        Some((top, self.saw_duplicate_keys))
    }

    fn parse_value(&mut self) -> Option<JsonTopKind> {
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => {
                self.parse_string()?;
                Some(JsonTopKind::Scalar)
            }
            b't' => {
                self.consume_exact(b"true")?;
                Some(JsonTopKind::Scalar)
            }
            b'f' => {
                self.consume_exact(b"false")?;
                Some(JsonTopKind::Scalar)
            }
            b'n' => {
                self.consume_exact(b"null")?;
                Some(JsonTopKind::Scalar)
            }
            b'-' | b'0'..=b'9' => {
                self.parse_number()?;
                Some(JsonTopKind::Scalar)
            }
            _ => None,
        }
    }

    fn parse_object(&mut self) -> Option<JsonTopKind> {
        self.expect_byte(b'{')?;
        self.skip_ws();
        if self.consume_byte(b'}') {
            return Some(JsonTopKind::Object);
        }
        let mut keys = HashSet::new();
        loop {
            let key = self.parse_string()?;
            if !keys.insert(key) {
                self.saw_duplicate_keys = true;
            }
            self.skip_ws();
            self.expect_byte(b':')?;
            self.skip_ws();
            self.parse_value()?;
            self.skip_ws();
            if self.consume_byte(b',') {
                self.skip_ws();
                continue;
            }
            self.expect_byte(b'}')?;
            break;
        }
        Some(JsonTopKind::Object)
    }

    fn parse_array(&mut self) -> Option<JsonTopKind> {
        self.expect_byte(b'[')?;
        self.skip_ws();
        if self.consume_byte(b']') {
            return Some(JsonTopKind::Array);
        }
        loop {
            self.parse_value()?;
            self.skip_ws();
            if self.consume_byte(b',') {
                self.skip_ws();
                continue;
            }
            self.expect_byte(b']')?;
            break;
        }
        Some(JsonTopKind::Array)
    }

    fn parse_string(&mut self) -> Option<String> {
        self.expect_byte(b'"')?;
        // Heuristic pre-size: cap at 64 bytes (typical JSON keys / short
        // string values fit). Caps remaining-input slack so we don't
        // over-allocate on a JSON document with one long header string.
        let remaining = self.bytes.len().saturating_sub(self.index);
        let mut out = String::with_capacity(remaining.min(64));
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    self.index += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.index += 1;
                    let esc = self.next_byte()?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.parse_hex_u16()?;
                            if (0xD800..=0xDBFF).contains(&first) {
                                let saved = self.index;
                                if self.expect_byte(b'\\').is_none()
                                    || self.expect_byte(b'u').is_none()
                                {
                                    return None;
                                }
                                let second = self.parse_hex_u16()?;
                                let combined = 0x10000
                                    + (((u32::from(first) - 0xD800) << 10)
                                        | (u32::from(second) - 0xDC00));
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return None;
                                }
                                if let Some(ch) = char::from_u32(combined) {
                                    out.push(ch);
                                } else {
                                    return None;
                                }
                                if self.index <= saved {
                                    return None;
                                }
                            } else if (0xDC00..=0xDFFF).contains(&first) {
                                return None;
                            } else if let Some(ch) = char::from_u32(u32::from(first)) {
                                out.push(ch);
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                }
                0x00..=0x1F => return None,
                _ => {
                    let remaining = std::str::from_utf8(&self.bytes[self.index..]).ok()?;
                    let ch = remaining.chars().next()?;
                    out.push(ch);
                    self.index += ch.len_utf8();
                }
            }
        }
    }

    fn parse_hex_u16(&mut self) -> Option<u16> {
        let end = self.index.checked_add(4)?;
        if end > self.bytes.len() {
            return None;
        }
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.next_byte()?;
            let nibble = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return None,
            };
            value = (value << 4) | u16::from(nibble);
        }
        Some(value)
    }

    fn parse_number(&mut self) -> Option<()> {
        if self.consume_byte(b'-') && !matches!(self.peek(), Some(b'0'..=b'9')) {
            return None;
        }
        match self.peek()? {
            b'0' => {
                self.index += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return None;
                }
            }
            b'1'..=b'9' => {
                self.index += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.index += 1;
                }
            }
            _ => return None,
        }
        if self.consume_byte(b'.') {
            let mut digits = 0_usize;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.index += 1;
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            let mut digits = 0_usize;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.index += 1;
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
        }
        Some(())
    }

    fn consume_exact(&mut self, expected: &[u8]) -> Option<()> {
        if self.bytes.get(self.index..self.index + expected.len())? != expected {
            return None;
        }
        self.index += expected.len();
        Some(())
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn expect_byte(&mut self, byte: u8) -> Option<()> {
        if self.consume_byte(byte) {
            Some(())
        } else {
            None
        }
    }

    fn consume_byte(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }
}

fn parse_json_predicate_kind(value: &Value) -> DbResult<JsonPredicateKind> {
    let raw = match value {
        Value::Text(text) => text.as_str(),
        _ => {
            return Err(DbError::internal(
                "__aiondb_is_json() expects text mode arguments",
            ));
        }
    };
    match raw.to_ascii_uppercase().as_str() {
        "JSON" | "VALUE" => Ok(JsonPredicateKind::Any),
        "OBJECT" => Ok(JsonPredicateKind::Object),
        "ARRAY" => Ok(JsonPredicateKind::Array),
        "SCALAR" => Ok(JsonPredicateKind::Scalar),
        _ => Err(DbError::internal(format!(
            "__aiondb_is_json() unknown predicate kind: {raw}"
        ))),
    }
}

fn parse_json_unique_mode(value: &Value) -> DbResult<JsonUniqueMode> {
    let raw = match value {
        Value::Text(text) => text.as_str(),
        _ => {
            return Err(DbError::internal(
                "__aiondb_is_json() expects text mode arguments",
            ));
        }
    };
    match raw.to_ascii_uppercase().as_str() {
        "DEFAULT" => Ok(JsonUniqueMode::Default),
        "WITH" => Ok(JsonUniqueMode::With),
        "WITHOUT" => Ok(JsonUniqueMode::Without),
        _ => Err(DbError::internal(format!(
            "__aiondb_is_json() unknown unique mode: {raw}"
        ))),
    }
}

fn json_top_kind(value: &serde_json::Value) -> JsonTopKind {
    match value {
        serde_json::Value::Object(_) => JsonTopKind::Object,
        serde_json::Value::Array(_) => JsonTopKind::Array,
        _ => JsonTopKind::Scalar,
    }
}

fn invalid_utf8_encoding_error(byte: u8) -> DbError {
    DbError::bind_error(
        SqlState::InvalidTextRepresentation,
        format!("invalid byte sequence for encoding \"UTF8\": 0x{byte:02x}"),
    )
}

fn decode_bytea_as_utf8(input: &[u8]) -> DbResult<&str> {
    if let Some(&bad) = input.iter().find(|&&byte| byte == 0) {
        return Err(invalid_utf8_encoding_error(bad));
    }
    match std::str::from_utf8(input) {
        Ok(text) => Ok(text),
        Err(err) => {
            let bad = input.get(err.valid_up_to()).copied().unwrap_or(0);
            Err(invalid_utf8_encoding_error(bad))
        }
    }
}

fn json_kind_matches(kind: JsonPredicateKind, top: JsonTopKind) -> bool {
    match kind {
        JsonPredicateKind::Any => true,
        JsonPredicateKind::Object => top == JsonTopKind::Object,
        JsonPredicateKind::Array => top == JsonTopKind::Array,
        JsonPredicateKind::Scalar => top == JsonTopKind::Scalar,
    }
}

pub(super) fn eval_is_json_predicate(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "__aiondb_is_json")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let predicate_kind = parse_json_predicate_kind(&args[1])?;
    let unique_mode = parse_json_unique_mode(&args[2])?;

    let (top_kind, has_duplicate_keys) = match &args[0] {
        Value::Jsonb(json) => (json_top_kind(json), false),
        Value::Text(text) => match JsonScanner::new(text).parse() {
            Some(result) => result,
            None => return Ok(Value::Boolean(false)),
        },
        Value::Blob(bytes) => {
            let text = decode_bytea_as_utf8(bytes)?;
            match JsonScanner::new(text).parse() {
                Some(result) => result,
                None => return Ok(Value::Boolean(false)),
            }
        }
        _ => {
            return Err(DbError::internal(
                "__aiondb_is_json() expects text/jsonb/bytea input",
            ));
        }
    };

    if unique_mode == JsonUniqueMode::With && has_duplicate_keys {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(json_kind_matches(predicate_kind, top_kind)))
}

pub(super) fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::BigInt(n) => serde_json::Value::Number((*n).into()),
        Value::Real(f) => serde_json::Number::from_f64(f64::from(*f))
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Double(d) => serde_json::Number::from_f64(*d)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Numeric(n) => {
            // Fast path: integer-valued NUMERIC with coefficient that
            // fits in i64 - render as a JSON integer without going
            // through `to_string` + parse. The dominant shape for
            // counter / id / row-count NUMERIC columns.
            if n.scale == 0 {
                if let Some(coeff) = n.try_coefficient_i128() {
                    if let Ok(i) = i64::try_from(coeff) {
                        return serde_json::Value::Number(i.into());
                    }
                }
            }
            // NaN / Infinity fall through to the legacy String form
            // (JSON has no representation for these). Otherwise use
            // direct f64 conversion via `to_f64`, skipping the
            // `to_string().parse::<f64>()` roundtrip.
            if n.is_nan() || n.is_infinite() {
                return serde_json::Value::String(n.to_string());
            }
            let f = n.to_f64();
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(n.to_string()))
        }
        Value::Text(s) => parse_pg_composite_text_to_json(s)
            .unwrap_or_else(|| serde_json::Value::String(s.clone())),
        Value::Jsonb(j) => j.clone(),
        Value::Array(arr) => serde_json::Value::Array(arr.iter().map(value_to_json).collect()),
        Value::Timestamp(dt) => {
            serde_json::Value::String(aiondb_core::temporal::format_timestamp_json(dt))
        }
        Value::TimestampTz(odt) => {
            serde_json::Value::String(aiondb_core::temporal::format_timestamptz_json(odt))
        }
        Value::Date(d) => serde_json::Value::String(aiondb_core::temporal::format_date(*d)),
        other => serde_json::Value::String(other.to_string()),
    }
}

fn parse_pg_composite_text_to_json(input: &str) -> Option<serde_json::Value> {
    if !looks_like_pg_composite(input) {
        return None;
    }
    let inner = &input[1..input.len() - 1];
    let fields = split_pg_composite_fields(inner)?;
    let mut object = serde_json::Map::new();
    for (index, field) in fields.into_iter().enumerate() {
        object.insert(
            format!("f{}", index + 1),
            parse_pg_composite_field_value(&field),
        );
    }
    Some(serde_json::Value::Object(object))
}

fn looks_like_pg_composite(input: &str) -> bool {
    input.starts_with('(') && input.ends_with(')') && input.contains(',')
}

fn split_pg_composite_fields(inner: &str) -> Option<Vec<String>> {
    // Pre-size by counting `,` bytes - upper bound on field count.
    let comma_count = inner.bytes().filter(|&b| b == b',').count();
    let mut fields = Vec::with_capacity(comma_count.saturating_add(1));
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_quotes {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            ',' => {
                fields.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if in_quotes || escaped {
        return None;
    }
    fields.push(current);
    Some(fields)
}

fn parse_pg_composite_field_value(field: &str) -> serde_json::Value {
    if field.is_empty() {
        return serde_json::Value::Null;
    }
    if let Some(object) = parse_pg_composite_text_to_json(field) {
        return object;
    }
    if let Some(array) = parse_pg_array_text_to_json(field) {
        return array;
    }
    if field.eq_ignore_ascii_case("null") {
        return serde_json::Value::Null;
    }
    if field.eq_ignore_ascii_case("t") {
        return serde_json::Value::Bool(true);
    }
    if field.eq_ignore_ascii_case("f") {
        return serde_json::Value::Bool(false);
    }
    if let Ok(number) = field.parse::<i64>() {
        return serde_json::Value::Number(number.into());
    }
    if let Ok(number) = field.parse::<f64>() {
        if let Some(json_number) = serde_json::Number::from_f64(number) {
            return serde_json::Value::Number(json_number);
        }
    }
    serde_json::Value::String(field.to_owned())
}

fn parse_pg_array_text_to_json(input: &str) -> Option<serde_json::Value> {
    if !(input.starts_with('{') && input.ends_with('}')) {
        return None;
    }
    let inner = &input[1..input.len() - 1];
    if inner.is_empty() {
        return Some(serde_json::Value::Array(Vec::new()));
    }
    // Pre-size by counting top-level commas. The walk handles
    // nested `{}` correctly, but the comma count is still an upper
    // bound on the element count - over-sized only by nested-array
    // commas, which is acceptable.
    let comma_count = inner.bytes().filter(|&b| b == b',').count();
    let mut values = Vec::with_capacity(comma_count.saturating_add(1));
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_quotes {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '{' => {
                depth = depth.saturating_add(1);
                current.push(ch);
            }
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                values.push(parse_pg_array_element(current.trim()));
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if in_quotes || escaped || depth != 0 {
        return None;
    }
    values.push(parse_pg_array_element(current.trim()));
    Some(serde_json::Value::Array(values))
}

fn parse_pg_array_element(element: &str) -> serde_json::Value {
    if element.is_empty() || element.eq_ignore_ascii_case("null") {
        return serde_json::Value::Null;
    }
    if let Some(array) = parse_pg_array_text_to_json(element) {
        return array;
    }
    if let Some(object) = parse_pg_composite_text_to_json(element) {
        return object;
    }
    parse_pg_composite_field_value(element)
}

const MAX_JSON_DEPTH: usize = 128;

pub(super) fn strip_nulls(val: &serde_json::Value) -> serde_json::Value {
    strip_nulls_inner(val, 0)
}

fn strip_nulls_inner(val: &serde_json::Value, depth: usize) -> serde_json::Value {
    if depth >= MAX_JSON_DEPTH {
        return val.clone();
    }
    match val {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                if !v.is_null() {
                    new_map.insert(k.clone(), strip_nulls_inner(v, depth + 1));
                }
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| strip_nulls_inner(v, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn eval_jsonb_delete(args: &[Value], path_mode: bool) -> DbResult<Value> {
    if args.len() < 2 {
        return Err(DbError::internal("jsonb_delete requires 2 arguments"));
    }
    let mut target = match &args[0] {
        Value::Jsonb(j) => j.clone(),
        Value::Null => return Ok(Value::Null),
        Value::Text(s) => {
            serde_json::from_str(s).map_err(|e| DbError::internal(format!("jsonb_delete: {e}")))?
        }
        _ => return Ok(Value::Null),
    };
    let is_scalar = !target.is_object() && !target.is_array();
    match &args[1] {
        Value::Text(key) => {
            if path_mode {
                if is_scalar {
                    return Err(DbError::internal("cannot delete path in scalar"));
                }
                let parts = parse_text_path(key);
                let path_parts: Vec<Cow<'_, str>> =
                    parts.iter().map(|p| Cow::Owned(p.clone())).collect();
                return Ok(Value::Jsonb(jsonb_delete_path_impl(target, &path_parts, 0)));
            }
            if is_scalar {
                return Err(DbError::internal("cannot delete from scalar"));
            }
            if let serde_json::Value::Object(map) = &mut target {
                map.remove(key);
            }
            Ok(Value::Jsonb(target))
        }
        Value::Int(idx) => {
            if is_scalar {
                return Err(DbError::internal("cannot delete from scalar"));
            }
            match &mut target {
                serde_json::Value::Array(arr) => {
                    if let Some(i) = normalized_json_index(arr.len(), i64::from(*idx)) {
                        if i < arr.len() {
                            arr.remove(i);
                        }
                    }
                }
                serde_json::Value::Object(_) => {
                    return Err(DbError::internal(
                        "cannot delete from object using integer index",
                    ));
                }
                _ => {}
            }
            Ok(Value::Jsonb(target))
        }
        Value::Array(keys) => {
            if path_mode {
                if is_scalar {
                    return Err(DbError::internal("cannot delete path in scalar"));
                }
                if keys.is_empty() {
                    return Ok(Value::Jsonb(target));
                }
                let path_parts: Vec<Cow<'_, str>> = keys.iter().map(json_path_component).collect();
                return Ok(Value::Jsonb(jsonb_delete_path_impl(target, &path_parts, 0)));
            }
            if is_scalar {
                return Err(DbError::internal("cannot delete from scalar"));
            }
            if keys.is_empty() {
                return Ok(Value::Jsonb(target));
            }
            if let serde_json::Value::Object(map) = &mut target {
                for k in keys {
                    if let Value::Text(key) = k {
                        map.remove(key);
                    }
                }
                return Ok(Value::Jsonb(target));
            }
            Ok(Value::Jsonb(target))
        }
        _ => Ok(Value::Jsonb(target)),
    }
}

fn parse_text_path(s: &str) -> Vec<String> {
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .unwrap_or(trimmed);
    if inner.is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|p| {
            let q = p.trim();
            q.strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(q)
                .to_string()
        })
        .collect()
}

fn json_path_component(value: &Value) -> Cow<'_, str> {
    match value {
        Value::Text(text) => Cow::Borrowed(text.as_str()),
        other => Cow::Owned(other.to_string()),
    }
}

fn jsonb_delete_path_impl(
    mut target: serde_json::Value,
    path: &[Cow<'_, str>],
    depth: usize,
) -> serde_json::Value {
    if path.is_empty() {
        return serde_json::Value::Null;
    }
    if depth >= MAX_JSON_DEPTH {
        return target;
    }
    let key = path[0].as_ref();
    let rest = &path[1..];
    match &mut target {
        serde_json::Value::Object(map) => {
            if rest.is_empty() {
                map.remove(key);
            } else if let Some(child) = map.remove(key) {
                map.insert(
                    key.to_owned(),
                    jsonb_delete_path_impl(child, rest, depth + 1),
                );
            }
            target
        }
        serde_json::Value::Array(arr) => {
            if let Ok(signed) = key.parse::<i64>() {
                if let Some(idx) = normalized_json_index(arr.len(), signed) {
                    if idx < arr.len() {
                        if rest.is_empty() {
                            arr.remove(idx);
                        } else {
                            let child = std::mem::replace(&mut arr[idx], serde_json::Value::Null);
                            arr[idx] = jsonb_delete_path_impl(child, rest, depth + 1);
                        }
                    }
                }
            }
            target
        }
        _ => target,
    }
}

pub(super) fn eval_jsonb_insert(args: &[Value]) -> DbResult<Value> {
    if args.len() < 3 {
        return Err(DbError::internal("jsonb_insert requires 3 or 4 arguments"));
    }
    let target = match &args[0] {
        Value::Jsonb(j) => j.clone(),
        Value::Text(s) => {
            serde_json::from_str(s).map_err(|e| DbError::internal(format!("jsonb_insert: {e}")))?
        }
        Value::Null => return Ok(Value::Null),
        _ => return Ok(Value::Null),
    };
    let path: Vec<Cow<'_, str>> = match &args[1] {
        Value::Text(s) => {
            let trimmed = s.trim();
            let inner = trimmed
                .strip_prefix('{')
                .and_then(|s2| s2.strip_suffix('}'))
                .unwrap_or(trimmed);
            if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').map(|p| Cow::Borrowed(p.trim())).collect()
            }
        }
        Value::Array(arr) => arr.iter().map(json_path_component).collect(),
        _ => return Ok(Value::Null),
    };
    let new_value_owned;
    let new_value = match &args[2] {
        Value::Jsonb(j) => j,
        Value::Text(s) => {
            new_value_owned = serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
            &new_value_owned
        }
        other => {
            new_value_owned = value_to_json(other);
            &new_value_owned
        }
    };
    let insert_after = args.len() > 3 && matches!(&args[3], Value::Boolean(true));
    Ok(Value::Jsonb(jsonb_insert_impl(
        target,
        &path,
        new_value,
        insert_after,
        0,
    )))
}

fn jsonb_insert_impl(
    mut target: serde_json::Value,
    path: &[Cow<'_, str>],
    new_value: &serde_json::Value,
    insert_after: bool,
    depth: usize,
) -> serde_json::Value {
    if path.is_empty() {
        return target;
    }
    if depth >= MAX_JSON_DEPTH {
        return target;
    }
    let key = path[0].as_ref();
    let rest = &path[1..];
    match &mut target {
        serde_json::Value::Object(map) => {
            if rest.is_empty() {
                if !map.contains_key(key) {
                    map.insert(key.to_owned(), new_value.clone());
                }
            } else if let Some(child) = map.remove(key) {
                map.insert(
                    key.to_owned(),
                    jsonb_insert_impl(child, rest, new_value, insert_after, depth + 1),
                );
            }
            target
        }
        serde_json::Value::Array(arr) => {
            if let Ok(idx) = key.parse::<i64>() {
                if rest.is_empty() {
                    if let Some(pos) = normalized_json_insert_index(arr.len(), idx, insert_after) {
                        arr.insert(pos, new_value.clone());
                    }
                    target
                } else {
                    if let Some(actual_idx) = normalized_json_index(arr.len(), idx) {
                        if actual_idx >= arr.len() {
                            return target;
                        }
                        let child =
                            std::mem::replace(&mut arr[actual_idx], serde_json::Value::Null);
                        arr[actual_idx] =
                            jsonb_insert_impl(child, rest, new_value, insert_after, depth + 1);
                    }
                    target
                }
            } else {
                target
            }
        }
        _ => target,
    }
}

pub(super) fn eval_jsonb_object(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Jsonb(serde_json::Value::Object(
            serde_json::Map::new(),
        )));
    }
    let coerced: Vec<Value> = args.iter().map(coerce_text_array_arg).collect();
    let args: &[Value] = &coerced;
    match args.len() {
        1 => {
            let arr = match &args[0] {
                Value::Array(a) => a,
                Value::Null => return Ok(Value::Null),
                _ => return Err(DbError::internal("jsonb_object requires text[] argument")),
            };
            if arr.len() % 2 != 0 {
                return Err(DbError::internal("array must have even number of elements"));
            }
            let mut map = serde_json::Map::new();
            for chunk in arr.chunks(2) {
                if matches!(chunk[0], Value::Null) {
                    return Err(DbError::internal("null value not allowed for object key"));
                }
                let key = match &chunk[0] {
                    Value::Text(s) => s.clone(),
                    other => other.to_string(),
                };
                let val = match &chunk[1] {
                    Value::Null => serde_json::Value::Null,
                    Value::Text(s) => serde_json::Value::String(s.clone()),
                    other => serde_json::Value::String(other.to_string()),
                };
                map.insert(key, val);
            }
            Ok(Value::Jsonb(serde_json::Value::Object(map)))
        }
        2 => {
            let keys = match &args[0] {
                Value::Array(a) => a,
                Value::Null => return Ok(Value::Null),
                _ => return Err(DbError::internal("jsonb_object requires text[] arguments")),
            };
            let vals = match &args[1] {
                Value::Array(a) => a,
                Value::Null => return Ok(Value::Null),
                _ => return Err(DbError::internal("jsonb_object requires text[] arguments")),
            };
            if keys.len() != vals.len() {
                return Err(DbError::internal("mismatched array dimensions"));
            }
            let mut map = serde_json::Map::new();
            for (k, v) in keys.iter().zip(vals.iter()) {
                if matches!(k, Value::Null) {
                    return Err(DbError::internal("null value not allowed for object key"));
                }
                let key = match k {
                    Value::Text(s) => s.clone(),
                    other => other.to_string(),
                };
                let val = match v {
                    Value::Null => serde_json::Value::Null,
                    Value::Text(s) => serde_json::Value::String(s.clone()),
                    other => serde_json::Value::String(other.to_string()),
                };
                map.insert(key, val);
            }
            Ok(Value::Jsonb(serde_json::Value::Object(map)))
        }
        _ => Err(DbError::internal("jsonb_object requires 1 or 2 arguments")),
    }
}

/// Coerce a text-form PG array literal (e.g. `'{a,b,c}'`) to a `Value::Array`
/// of text elements. Other values pass through untouched. PostgreSQL applies
/// this implicit text→text[] cast at call sites; we mirror it here so
/// `jsonb_object('{a,b}')` works without an explicit `::text[]` cast.
fn coerce_text_array_arg(v: &Value) -> Value {
    if let Value::Text(s) = v {
        let trimmed = s.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.len() >= 2 {
            let inner = &trimmed[1..trimmed.len() - 1];
            if inner.is_empty() {
                return Value::Array(Vec::new());
            }
            let mut items = Vec::new();
            let bytes = inner.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    let mut buf = String::new();
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            buf.push(bytes[i + 1] as char);
                            i += 2;
                        } else {
                            buf.push(bytes[i] as char);
                            i += 1;
                        }
                    }
                    items.push(Value::Text(buf));
                    if i < bytes.len() && bytes[i] == b'"' {
                        i += 1;
                    }
                    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b',') {
                        i += 1;
                    }
                } else {
                    let start = i;
                    while i < bytes.len() && bytes[i] != b',' {
                        i += 1;
                    }
                    let raw = inner[start..i].trim();
                    if raw.eq_ignore_ascii_case("NULL") {
                        items.push(Value::Null);
                    } else {
                        items.push(Value::Text(raw.to_owned()));
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            return Value::Array(items);
        }
    }
    v.clone()
}

// ── Helpers shared with jsonpath.rs ─────────────────────────────────

pub(super) fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

pub(super) fn whole_f64_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() || value.fract() != 0.0 || value.abs() >= i64_to_f64(i64::MAX) {
        return None;
    }
    let rendered = format!("{value:.0}");
    rendered.parse::<i64>().ok()
}

pub(super) fn floor_clamped_f64_to_i64(value: f64) -> i64 {
    let floored = value
        .floor()
        .clamp(i64_to_f64(i64::MIN), i64_to_f64(i64::MAX));
    f64_to_i64(floored).unwrap_or_else(|_| {
        if floored.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}
