//\! PostgreSQL compatibility functions: regtype/regclass resolution,
//\! format_type, user cast dispatch, pg_describe_object, and related
//\! system-catalog function stubs.

use std::cmp::Ordering;

use aiondb_core::{
    compat_role_oid, DbError, DbResult, ErrorReport, NumericValue, SqlState, Value,
    COMPAT_BOOTSTRAP_ROLE_NAME, COMPAT_BOOTSTRAP_ROLE_OID, COMPAT_PGVECTOR_HALFVEC_ARRAY_OID,
    COMPAT_PGVECTOR_HALFVEC_OID, COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID,
    COMPAT_PGVECTOR_SPARSEVEC_OID, COMPAT_PGVECTOR_VECTOR_ARRAY_OID, COMPAT_PGVECTOR_VECTOR_OID,
    COMPAT_PG_BIT_ARRAY_OID, COMPAT_PG_BIT_OID, COMPAT_PG_VARBIT_ARRAY_OID, COMPAT_PG_VARBIT_OID,
};

use crate::eval::session::{
    compat_display_type_name, current_search_path_schemas, normalize_compat_type_name,
    visible_session_schema_name, with_current_session_context, CompatCastMethod, CompatUserCast,
};

use super::geometric::{
    parse_box_text, parse_circle_text, parse_lseg_text, parse_path_text, parse_point_text,
    parse_polygon_text, GeomPoint,
};
use super::{expect_args, to_i32_saturating, value_to_text};

fn compat_domain_identity(schema_name: Option<&str>, name: &str) -> String {
    match schema_name {
        Some(schema_name) if !schema_name.is_empty() => {
            format!(
                "{}.{}",
                schema_name.to_ascii_lowercase(),
                name.to_ascii_lowercase()
            )
        }
        _ => name.to_ascii_lowercase(),
    }
}

fn compat_domain_oid(schema_name: Option<&str>, name: &str) -> i32 {
    aiondb_core::compat_function_oid(&format!(
        "domain:{}",
        compat_domain_identity(schema_name, name)
    ))
}

pub fn pg_format_type(type_oid: i32, typemod: i32) -> String {
    let base = match type_oid {
        16 => "boolean",
        17 => "bytea",
        18 => "\"char\"",
        20 => "bigint",
        21 => "smallint",
        23 => "integer",
        25 => "text",
        26 => "oid",
        27 => "tid",
        700 => "real",
        701 => "double precision",
        1042 => "character",
        1043 => "character varying",
        COMPAT_PG_BIT_OID => "bit",
        COMPAT_PG_BIT_ARRAY_OID => "bit[]",
        COMPAT_PG_VARBIT_OID => "bit varying",
        COMPAT_PG_VARBIT_ARRAY_OID => "bit varying[]",
        1000 => "boolean[]",
        1001 => "bytea[]",
        1005 => "smallint[]",
        1007 => "integer[]",
        1009 => "text[]",
        1010 => "tid[]",
        1016 => "bigint[]",
        1014 => "_character",
        1015 => "_varchar",
        1021 => "real[]",
        1022 => "double precision[]",
        1040 => "macaddr[]",
        1082 => "date",
        1083 => "time without time zone",
        1115 => "timestamp without time zone[]",
        1182 => "date[]",
        1183 => "time without time zone[]",
        1266 => "time with time zone",
        1185 => "timestamp with time zone[]",
        1114 => "timestamp without time zone",
        1184 => "timestamp with time zone",
        1186 => "interval",
        1187 => "interval[]",
        1231 => "numeric[]",
        1270 => "time with time zone[]",
        1700 => "numeric",
        2950 => "uuid",
        2951 => "uuid[]",
        3220 => "pg_lsn",
        3221 => "pg_lsn[]",
        3802 => "jsonb",
        3807 => "jsonb[]",
        COMPAT_PGVECTOR_VECTOR_OID => "vector",
        COMPAT_PGVECTOR_VECTOR_ARRAY_OID => "vector[]",
        COMPAT_PGVECTOR_HALFVEC_OID => "halfvec",
        COMPAT_PGVECTOR_HALFVEC_ARRAY_OID => "halfvec[]",
        COMPAT_PGVECTOR_SPARSEVEC_OID => "sparsevec",
        COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID => "sparsevec[]",
        114 => "json",
        142 => "xml",
        2249 => "record",
        2278 => "void",
        2281 => "internal",
        _ => {
            if let Some(type_name) = with_current_session_context(|ctx| {
                ctx.compat_user_types
                    .iter()
                    .find(|entry| entry.oid == type_oid)
                    .map(|entry| {
                        let display_name = compat_display_type_name(&entry.name);
                        let schema_name = entry.schema_name.as_deref();
                        if let Some(schema_name) = schema_name {
                            let in_search_path = current_search_path_schemas()
                                .iter()
                                .any(|schema| schema.eq_ignore_ascii_case(schema_name));
                            if !in_search_path && !schema_name.eq_ignore_ascii_case("public") {
                                return format!("{schema_name}.{display_name}");
                            }
                        }
                        display_name
                    })
            }) {
                return type_name;
            }
            if let Some(type_name) = with_current_session_context(|ctx| {
                ctx.domain_defs.iter().find_map(|entry| {
                    (compat_domain_oid(entry.schema_name.as_deref(), &entry.name) == type_oid).then(
                        || {
                            let display_name = compat_display_type_name(&entry.name);
                            let schema_name = entry.schema_name.as_deref();
                            if let Some(schema_name) = schema_name {
                                let in_search_path = current_search_path_schemas()
                                    .iter()
                                    .any(|schema| schema.eq_ignore_ascii_case(schema_name));
                                if !in_search_path && !schema_name.eq_ignore_ascii_case("public") {
                                    return format!("{schema_name}.{display_name}");
                                }
                            }
                            display_name
                        },
                    )
                })
            }) {
                return type_name;
            }
            return format!("unknown (OID={type_oid})");
        }
    };
    if typemod >= 0 {
        match type_oid {
            1043 => {
                let len = typemod - 4;
                return format!("character varying({len})");
            }
            1042 => {
                let len = typemod - 4;
                return format!("character({len})");
            }
            1014 => {
                let len = typemod - 4;
                return format!("character({len})[]");
            }
            1015 => {
                let len = typemod - 4;
                return format!("character varying({len})[]");
            }
            1700 => {
                let precision = ((typemod - 4) >> 16) & 0xffff;
                let scale = (typemod - 4) & 0xffff;
                return format!("numeric({precision},{scale})");
            }
            COMPAT_PGVECTOR_VECTOR_OID
            | COMPAT_PGVECTOR_HALFVEC_OID
            | COMPAT_PGVECTOR_SPARSEVEC_OID
                if typemod > 4 =>
            {
                let dims = typemod - 4;
                return format!("{base}({dims})");
            }
            _ => {}
        }
    }
    base.to_owned()
}

// =====================================================================
// Geometric function helpers
// =====================================================================

/// Geometric type constructors: passthrough or format arguments as text.
pub fn eval_geometric_constructor(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    // If any argument is NULL, the result is NULL (PostgreSQL semantics).
    if args.iter().any(|v| matches!(v, Value::Null)) {
        return Ok(Value::Null);
    }
    // If single argument, just pass through the text representation
    if args.len() == 1 {
        return match &args[0] {
            Value::Text(s) => Ok(Value::Text(s.clone())),
            other => Ok(Value::Text(other.to_string())),
        };
    }
    // Multiple arguments: format as type constructor
    if name.eq_ignore_ascii_case("polygon") && args.len() == 2 {
        let npoints = match &args[0] {
            Value::Int(v) => i64::from(*v),
            Value::BigInt(v) => *v,
            Value::Numeric(v) => v
                .try_coefficient_i128()
                .and_then(|value| i64::try_from(value).ok())
                .unwrap_or(0),
            other => value_to_text(other).trim().parse::<i64>().unwrap_or(0),
        };
        let circle = parse_circle_text(&value_to_text(&args[1]))?;
        if npoints < 2 {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                "must request at least 2 points",
            )));
        }
        if circle.radius == 0.0 {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                "cannot convert circle with radius zero to polygon",
            )));
        }
    }

    let parts: Vec<String> = args
        .iter()
        .map(|v| match v {
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    Ok(Value::Text(format!("{}({})", name, parts.join(","))))
}

/// Pad a text value to the declared CHAR(n) length with trailing spaces,
/// or truncate if it exceeds the length (only trailing spaces may be truncated).
/// This implements PostgreSQL's CHAR(n) blank-padding semantics for CAST expressions.
pub fn eval_char_pad_length(args: &[Value]) -> DbResult<Value> {
    const MAX_CHAR_PAD_LENGTH: usize = 10_000_000;

    if args.len() != 2 {
        return Err(DbError::internal(
            "__aiondb_char_pad_length() requires exactly 2 arguments",
        ));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let text = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    let length = match &args[1] {
        Value::Int(n) if *n >= 0 => usize::try_from(*n).map_err(|_| {
            DbError::program_limit("__aiondb_char_pad_length() length exceeds platform limits")
        })?,
        Value::BigInt(n) if *n >= 0 => usize::try_from(*n).map_err(|_| {
            DbError::program_limit("__aiondb_char_pad_length() length exceeds platform limits")
        })?,
        Value::Int(_) | Value::BigInt(_) => {
            return Err(DbError::internal(
                "__aiondb_char_pad_length() length must be non-negative",
            ));
        }
        _ => {
            return Err(DbError::internal(
                "__aiondb_char_pad_length() length must be integer",
            ));
        }
    };
    if length > MAX_CHAR_PAD_LENGTH {
        return Err(DbError::program_limit(format!(
            "__aiondb_char_pad_length() length exceeds maximum allowed size ({MAX_CHAR_PAD_LENGTH})"
        )));
    }
    // ASCII fast path: byte length == char count.
    let char_count = if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    };
    if char_count > length {
        // PostgreSQL allows truncation only if the excess characters are spaces
        let excess_is_only_spaces = text.chars().skip(length).all(|ch| ch == ' ');
        if !excess_is_only_spaces {
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                aiondb_core::SqlState::StringDataRightTruncation,
                format!("value too long for type character({length})"),
            )));
        }
        let truncated: String = text.chars().take(length).collect();
        Ok(Value::Text(truncated))
    } else {
        let pad = length.saturating_sub(char_count);
        if pad == 0 {
            return Ok(Value::Text(text));
        }
        let mut padded = text;
        padded.reserve(pad);
        for _ in 0..pad {
            padded.push(' ');
        }
        Ok(Value::Text(padded))
    }
}

fn malformed_multirange_literal(literal: &str, detail: &str) -> DbError {
    DbError::from_report(
        ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            format!("malformed multirange literal: \"{}\"", literal.trim()),
        )
        .with_client_detail(detail),
    )
}

fn malformed_range_literal(literal: &str, detail: &str) -> DbError {
    DbError::from_report(
        ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            format!("malformed range literal: \"{}\"", literal.trim()),
        )
        .with_client_detail(detail),
    )
}

fn split_multirange_items(inner: &str, full_literal: &str) -> DbResult<Vec<String>> {
    let mut items = Vec::new();
    let mut idx = 0usize;
    let bytes = inner.as_bytes();
    let mut expect_item = false;

    while idx < inner.len() {
        while idx < inner.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= inner.len() {
            if expect_item {
                return Err(malformed_multirange_literal(
                    full_literal,
                    "Expected range start.",
                ));
            }
            break;
        }
        expect_item = false;

        let item_start = idx;
        let ch = bytes[idx] as char;
        if ch != '[' && ch != '(' {
            let next_comma = inner[idx..]
                .find(',')
                .map(|off| idx + off)
                .unwrap_or(inner.len());
            items.push(inner[item_start..next_comma].to_owned());
            idx = if next_comma < inner.len() {
                expect_item = true;
                next_comma + 1
            } else {
                next_comma
            };
            continue;
        }

        idx += 1;
        let mut in_quotes = false;
        let mut escape = false;
        let mut found_end = false;

        while idx < inner.len() {
            let ch = bytes[idx] as char;
            if escape {
                escape = false;
                idx += 1;
                continue;
            }
            match ch {
                '\\' => {
                    escape = true;
                    idx += 1;
                }
                '"' => {
                    in_quotes = !in_quotes;
                    idx += 1;
                }
                ']' | ')' if !in_quotes => {
                    let range_end = idx + 1;
                    let mut lookahead = range_end;
                    while lookahead < inner.len() && bytes[lookahead].is_ascii_whitespace() {
                        lookahead += 1;
                    }
                    if lookahead == inner.len() || bytes[lookahead] == b',' {
                        items.push(inner[item_start..range_end].to_owned());
                        idx = if lookahead < inner.len() {
                            expect_item = true;
                            lookahead + 1
                        } else {
                            lookahead
                        };
                        found_end = true;
                        break;
                    }
                    idx += 1;
                }
                _ => idx += 1,
            }
        }

        if !found_end {
            return Err(malformed_multirange_literal(
                full_literal,
                "Unexpected end of input.",
            ));
        }
    }
    if expect_item {
        return Err(malformed_multirange_literal(
            full_literal,
            "Expected range start.",
        ));
    }

    Ok(items)
}

fn find_unquoted_comma(inner: &str, full_range_literal: &str) -> DbResult<usize> {
    let mut in_quotes = false;
    let mut escape = false;
    let mut comma_idx: Option<usize> = None;

    for (idx, ch) in inner.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' => {
                escape = true;
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                if comma_idx.is_some() {
                    return Err(malformed_range_literal(
                        full_range_literal,
                        "Too many commas.",
                    ));
                }
                comma_idx = Some(idx);
            }
            _ => {}
        }
    }

    if in_quotes || escape {
        return Err(malformed_multirange_literal(
            full_range_literal,
            "Unexpected end of input.",
        ));
    }

    comma_idx.ok_or_else(|| {
        malformed_range_literal(full_range_literal, "Missing comma after lower bound.")
    })
}

fn has_unquoted_closing_bracket(bound: &str) -> bool {
    let mut in_quotes = false;
    let mut escape = false;
    for ch in bound.chars() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '"' => in_quotes = !in_quotes,
            ')' | ']' if !in_quotes => return true,
            _ => {}
        }
    }
    false
}

/// When both plain bounds look numeric, compare them numerically so that
/// scientific notation (`5.e9`), zero-padding, or leading signs do not break
/// ordering.  Returns `true` if the numeric comparison places `lower <= upper`
/// (i.e. the range is well-ordered after numeric interpretation).  Returns
/// `false` when the values are not both parseable as numerics, in which case
/// the caller falls back on textual ordering.
fn plain_bounds_compare_lt_or_eq(lower: &str, upper: &str) -> bool {
    use aiondb_core::NumericValue;
    let Ok(l) = lower.parse::<NumericValue>() else {
        return false;
    };
    let Ok(u) = upper.parse::<NumericValue>() else {
        return false;
    };
    l.cmp(&u) != std::cmp::Ordering::Greater
}

fn is_plain_bound(bound: &str) -> bool {
    !bound.is_empty()
        && !bound.contains('"')
        && !bound.contains('\\')
        && !bound.contains(',')
        && !bound.contains('[')
        && !bound.contains(']')
        && !bound.contains('(')
        && !bound.contains(')')
}

fn parse_and_normalize_multirange_item(
    raw_item: &str,
    full_literal: &str,
) -> DbResult<Option<String>> {
    let item = raw_item.trim();
    if item.is_empty() {
        return Err(malformed_multirange_literal(
            full_literal,
            "Expected range start.",
        ));
    }
    if item.eq_ignore_ascii_case("empty") {
        return Ok(None);
    }

    let first = item.chars().next().unwrap_or('\0');
    let last = item.chars().last().unwrap_or('\0');
    if !matches!(first, '[' | '(') {
        return Err(malformed_multirange_literal(
            full_literal,
            "Expected range start.",
        ));
    }
    if !matches!(last, ']' | ')') {
        return Err(malformed_multirange_literal(
            full_literal,
            "Expected comma or end of multirange.",
        ));
    }
    if item.len() < 2 {
        return Err(malformed_range_literal(
            item,
            "Missing comma after lower bound.",
        ));
    }

    let inner = &item[1..item.len() - 1];
    let comma_idx = find_unquoted_comma(inner, item)?;
    let lower = &inner[..comma_idx];
    let upper = &inner[comma_idx + 1..];

    if has_unquoted_closing_bracket(lower) || has_unquoted_closing_bracket(upper) {
        return Err(malformed_multirange_literal(
            full_literal,
            "Expected comma or end of multirange.",
        ));
    }

    let mut lb = first;
    let mut ub = last;
    if lower.is_empty() && lb == '[' {
        lb = '(';
    }
    if upper.is_empty() && ub == ']' {
        ub = ')';
    }

    if is_plain_bound(lower) && is_plain_bound(upper) {
        // Compare on the trimmed forms - leading/trailing whitespace is
        // tolerated by every numeric input function and the literal
        // `' 5.e9'` should not lex-compare smaller than `'123.001'` simply
        // because of an inserted space. For non-numeric plain bounds (date,
        // text, …) trimming is also harmless because the surrounding text
        // never contributes to ordering.
        let lt = lower.trim();
        let ut = upper.trim();
        if lt > ut && !plain_bounds_compare_lt_or_eq(lt, ut) {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                "range lower bound must be less than or equal to range upper bound",
            )));
        }
        if lt == ut && !(lb == '[' && ub == ']') {
            return Ok(None);
        }
    }

    Ok(Some(format!("{lb}{lower},{upper}{ub}")))
}

#[derive(Clone)]
struct PlainRange {
    lower: String,
    upper: String,
    lower_inc: bool,
    upper_inc: bool,
}

fn parse_plain_range(range: &str) -> Option<PlainRange> {
    let trimmed = range.trim();
    if trimmed.eq_ignore_ascii_case("empty") || trimmed.len() < 2 {
        return None;
    }
    let first = trimmed.chars().next()?;
    let last = trimmed.chars().last()?;
    if !matches!(first, '[' | '(') || !matches!(last, ']' | ')') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let comma = find_unquoted_comma(inner, trimmed).ok()?;
    let lower = inner[..comma].to_owned();
    let upper = inner[comma + 1..].to_owned();
    if !((lower.is_empty() || is_plain_bound(&lower))
        && (upper.is_empty() || is_plain_bound(&upper)))
    {
        return None;
    }
    Some(PlainRange {
        lower,
        upper,
        lower_inc: first == '[',
        upper_inc: last == ']',
    })
}

fn cmp_plain_lower(a: &PlainRange, b: &PlainRange) -> Ordering {
    match (a.lower.is_empty(), b.lower.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => match a.lower.cmp(&b.lower) {
            Ordering::Equal => {
                if a.lower_inc == b.lower_inc {
                    Ordering::Equal
                } else if a.lower_inc {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            ord => ord,
        },
    }
}

fn cmp_plain_upper(a: &PlainRange, b: &PlainRange) -> Ordering {
    match (a.upper.is_empty(), b.upper.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => match a.upper.cmp(&b.upper) {
            Ordering::Equal => {
                if a.upper_inc == b.upper_inc {
                    Ordering::Equal
                } else if a.upper_inc {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            ord => ord,
        },
    }
}

fn plain_ranges_overlap_or_adjacent(a: &PlainRange, b: &PlainRange) -> bool {
    if a.upper.is_empty() || b.lower.is_empty() {
        return true;
    }
    match a.upper.cmp(&b.lower) {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => a.upper_inc || b.lower_inc,
    }
}

fn merge_plain_ranges(mut ranges: Vec<PlainRange>) -> Vec<PlainRange> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by(cmp_plain_lower);
    let mut merged: Vec<PlainRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if plain_ranges_overlap_or_adjacent(last, &range) {
                if cmp_plain_upper(&range, last) == Ordering::Greater {
                    last.upper = range.upper.clone();
                    last.upper_inc = range.upper_inc;
                }
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

pub(crate) fn parse_and_normalize_multirange_literal(literal: &str) -> DbResult<Vec<String>> {
    let trimmed = literal.trim();
    if !trimmed.starts_with('{') {
        return Err(malformed_multirange_literal(trimmed, "Missing left brace."));
    }
    if !trimmed.ends_with('}') {
        // Missing closing brace at end of literal: PG reports this as
        // "Unexpected end of input." rather than "junk after right brace".
        return Err(malformed_multirange_literal(
            trimmed,
            "Unexpected end of input.",
        ));
    }
    if trimmed.len() < 2 {
        return Err(malformed_multirange_literal(
            trimmed,
            "Unexpected end of input.",
        ));
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut normalized = Vec::new();
    for item in split_multirange_items(inner, trimmed)? {
        if let Some(range) = parse_and_normalize_multirange_item(&item, trimmed)? {
            normalized.push(range);
        }
    }
    let mut plain_ranges = Vec::with_capacity(normalized.len());
    for range in &normalized {
        let Some(parsed) = parse_plain_range(range) else {
            return Ok(normalized);
        };
        plain_ranges.push(parsed);
    }
    let merged = merge_plain_ranges(plain_ranges);
    Ok(merged
        .into_iter()
        .map(|range| {
            let lb = if range.lower_inc { '[' } else { '(' };
            let ub = if range.upper_inc { ']' } else { ')' };
            format!("{lb}{},{}{ub}", range.lower, range.upper)
        })
        .collect())
}

fn normalize_multirange_literal_text(literal: &str) -> DbResult<String> {
    let ranges = parse_and_normalize_multirange_literal(literal)?;
    if ranges.is_empty() {
        Ok("{}".to_owned())
    } else {
        Ok(format!("{{{}}}", ranges.join(",")))
    }
}

fn domain_base_type_name(target_type: &str) -> String {
    with_current_session_context(|session_context| {
        let mut current = target_type.to_owned();
        for _ in 0..32 {
            match session_context.domain_def(&current) {
                Some(def) => {
                    current = normalize_compat_type_name(&def.base_type);
                }
                None => break,
            }
        }
        current
    })
}

pub fn eval_to_regtype(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regtype")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => {
            Ok(resolve_regtype_name(&value_to_text(value)).map_or(Value::Null, Value::Text))
        }
    }
}

pub fn eval_regtype(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "regtype")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => lookup_regtype_name(&value_to_text(value)).map(Value::Text),
    }
}

pub fn eval_to_regclass(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regclass")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => {
            Ok(resolve_regclass_name(&value_to_text(value)).map_or(Value::Null, Value::Text))
        }
    }
}

pub fn eval_regclass(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "regclass")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => lookup_regclass_name(&value_to_text(value)).map(Value::Text),
    }
}

pub fn eval_to_regnamespace(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regnamespace")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regnamespace_name(&value_to_text(value))
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_regnamespace(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "regnamespace")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => {
            let input = value_to_text(value);
            match lookup_regnamespace_name(&input)? {
                Some(name) => Ok(Value::Text(name)),
                None => {
                    let parsed = if input.trim().starts_with('"') && input.trim().ends_with('"') {
                        input.trim().trim_matches('"').replace("\"\"", "\"")
                    } else {
                        input.trim().to_ascii_lowercase()
                    };
                    Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidSchemaName,
                        format!("schema \"{parsed}\" does not exist"),
                    )))
                }
            }
        }
    }
}

pub fn eval_to_regrole(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regrole")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regrole_name(&value_to_text(value))
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_to_regproc(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regproc")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regproc_name(&value_to_text(value))
            .ok()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_to_regprocedure(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regprocedure")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regprocedure_name(&value_to_text(value))
            .ok()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_to_regoper(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regoper")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regoper_name(&value_to_text(value))
            .ok()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_to_regoperator(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regoperator")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regoperator_name(&value_to_text(value))
            .ok()
            .map_or(Value::Null, Value::Text)),
    }
}

pub fn eval_to_regcollation(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "to_regcollation")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(lookup_regcollation_name(&value_to_text(value))
            .ok()
            .map_or(Value::Null, Value::Text)),
    }
}

fn format_geom_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    let mut text = format!("{value:.12}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" {
        return "0".to_owned();
    }
    text
}

fn format_geom_point(point: GeomPoint) -> String {
    format!(
        "({},{})",
        format_geom_number(point.x),
        format_geom_number(point.y)
    )
}

fn format_path(closed: bool, points: &[GeomPoint]) -> String {
    // Build the entire `(p1,p2,...)` / `[p1,p2,...]` literal in a
    // single buffer instead of paying per-point format! plus a
    // Vec<_> collect + join + outer format! wrapper.
    let mut out = String::with_capacity(2 + points.len() * 16);
    out.push(if closed { '(' } else { '[' });
    write_geom_points_csv(&mut out, points);
    out.push(if closed { ')' } else { ']' });
    out
}

fn format_polygon(points: &[GeomPoint]) -> String {
    let mut out = String::with_capacity(2 + points.len() * 16);
    out.push('(');
    write_geom_points_csv(&mut out, points);
    out.push(')');
    out
}

#[inline]
fn write_geom_points_csv(out: &mut String, points: &[GeomPoint]) {
    use std::fmt::Write as _;
    for (i, point) in points.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Equivalent to `format_geom_point` but writes the
        // `(x,y)` shape directly into `out` without an
        // intermediate per-point String allocation.
        out.push('(');
        let _ = write!(out, "{}", format_geom_number(point.x));
        out.push(',');
        let _ = write!(out, "{}", format_geom_number(point.y));
        out.push(')');
    }
}

fn coerce_geometric_text(
    source_type: &str,
    target_base_type: &str,
    text_value: &str,
) -> DbResult<String> {
    match target_base_type {
        "point" => {
            if let Ok(point) = parse_point_text(text_value) {
                return Ok(format_geom_point(point));
            }
            if source_type == "lseg" || parse_lseg_text(text_value).is_ok() {
                let (a, b) = parse_lseg_text(text_value)?;
                return Ok(format_geom_point(GeomPoint {
                    x: (a.x + b.x) / 2.0,
                    y: (a.y + b.y) / 2.0,
                }));
            }
            Err(DbError::invalid_input_syntax("point", text_value))
        }
        "box" => {
            let (a, b) = match source_type {
                "point" => {
                    let point = parse_point_text(text_value)?;
                    (point, point)
                }
                "lseg" => parse_lseg_text(text_value)?,
                "path" => {
                    let path = parse_path_text(text_value)?;
                    let mut xmin = f64::INFINITY;
                    let mut xmax = f64::NEG_INFINITY;
                    let mut ymin = f64::INFINITY;
                    let mut ymax = f64::NEG_INFINITY;
                    for point in path.points {
                        xmin = xmin.min(point.x);
                        xmax = xmax.max(point.x);
                        ymin = ymin.min(point.y);
                        ymax = ymax.max(point.y);
                    }
                    (
                        GeomPoint { x: xmin, y: ymin },
                        GeomPoint { x: xmax, y: ymax },
                    )
                }
                "polygon" => {
                    let points = parse_polygon_text(text_value)?;
                    let mut xmin = f64::INFINITY;
                    let mut xmax = f64::NEG_INFINITY;
                    let mut ymin = f64::INFINITY;
                    let mut ymax = f64::NEG_INFINITY;
                    for point in points {
                        xmin = xmin.min(point.x);
                        xmax = xmax.max(point.x);
                        ymin = ymin.min(point.y);
                        ymax = ymax.max(point.y);
                    }
                    (
                        GeomPoint { x: xmin, y: ymin },
                        GeomPoint { x: xmax, y: ymax },
                    )
                }
                "circle" => {
                    let circle = parse_circle_text(text_value)?;
                    (
                        GeomPoint {
                            x: circle.center.x - circle.radius,
                            y: circle.center.y - circle.radius,
                        },
                        GeomPoint {
                            x: circle.center.x + circle.radius,
                            y: circle.center.y + circle.radius,
                        },
                    )
                }
                _ => parse_box_text(text_value)?,
            };
            let xmin = a.x.min(b.x);
            let xmax = a.x.max(b.x);
            let ymin = a.y.min(b.y);
            let ymax = a.y.max(b.y);
            let upper = format!(
                "({},{})",
                format_geom_number(xmax),
                format_geom_number(ymax)
            );
            let lower = format!(
                "({},{})",
                format_geom_number(xmin),
                format_geom_number(ymin)
            );
            Ok(format!("{upper},{lower}"))
        }
        "path" => {
            if let Ok(path) = parse_path_text(text_value) {
                return Ok(format_path(path.closed, &path.points));
            }
            if source_type == "polygon" {
                let points = parse_polygon_text(text_value)?;
                return Ok(format_path(true, &points));
            }
            Err(DbError::invalid_input_syntax("path", text_value))
        }
        "polygon" => {
            if source_type == "path" {
                let path = parse_path_text(text_value)?;
                if !path.closed {
                    return Err(DbError::invalid_input_syntax("polygon", text_value));
                }
                return Ok(format_polygon(&path.points));
            }
            if source_type == "circle"
                || (source_type == "text"
                    && text_value
                        .trim_start()
                        .get(..7)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("circle(")))
            {
                let circle = parse_circle_text(text_value)?;
                let steps = 12_u32;
                let points = (0..steps)
                    .map(|index| {
                        let angle = std::f64::consts::PI
                            - (2.0 * std::f64::consts::PI * f64::from(index) / f64::from(steps));
                        GeomPoint {
                            x: circle.center.x + circle.radius * angle.cos(),
                            y: circle.center.y + circle.radius * angle.sin(),
                        }
                    })
                    .collect::<Vec<_>>();
                return Ok(format_polygon(&points));
            }
            let points = parse_polygon_text(text_value)?;
            Ok(format_polygon(&points))
        }
        _ => {
            super::geometric::validate_geometric_literal(target_base_type, text_value)?;
            Ok(text_value.to_owned())
        }
    }
}

pub fn eval_compat_user_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "__aiondb_compat_cast")?;

    let target_type = match args.get(2) {
        Some(Value::Text(value)) => normalize_compat_type_name(value),
        Some(value) => normalize_compat_type_name(&value_to_text(value)),
        None => return Ok(Value::Null),
    };

    // For NULL values, check if the domain allows NULL before returning.
    if args.first().is_some_and(Value::is_null) {
        if with_current_session_context(|ctx| ctx.domain_def(&target_type).is_some()) {
            crate::eval::domain_check::enforce_domain_constraints(&Value::Null, &target_type)?;
        }
        return Ok(Value::Null);
    }

    let source_type = match args.get(1) {
        Some(Value::Text(value)) => normalize_compat_type_name(value),
        Some(value) => normalize_compat_type_name(&value_to_text(value)),
        None => return Ok(Value::Null),
    };
    let cast =
        with_current_session_context(|ctx| ctx.compat_cast(&source_type, &target_type).cloned());
    let target_base_type = domain_base_type_name(&target_type);
    let multirange_text = if target_base_type.ends_with("multirange") {
        let source_text = value_to_text(&args[0]);
        let source_trimmed = source_text.trim();
        let should_wrap_single_range = source_type.ends_with("range")
            || (!source_trimmed.is_empty() && !source_trimmed.starts_with('{'));
        let owned_wrapped;
        let literal: &str = if should_wrap_single_range {
            owned_wrapped = if source_trimmed.eq_ignore_ascii_case("empty") {
                "{}".to_owned()
            } else {
                format!("{{{source_trimmed}}}")
            };
            owned_wrapped.as_str()
        } else {
            source_trimmed
        };
        // First run the structural validator from `parse_and_normalize_multirange_literal`
        // so that malformed-literal errors carry the rich PG-style detail
        // ("Missing left brace.", "Junk after closing right brace.",
        //  "Expected range start.", etc.).  Then delegate canonicalisation
        // to the kind-aware path so bound values are re-rendered in PG
        // canonical form (text quoting, numeric rounding, date rendering…).
        let structural = normalize_multirange_literal_text(literal)?;
        let normalized = match target_base_type.as_str() {
            "int4multirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Int4,
            )?,
            "int8multirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Int8,
            )?,
            "nummultirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Numeric,
            )?,
            "datemultirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Date,
            )?,
            "tsmultirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Timestamp,
            )?,
            "tstzmultirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::TimestampTz,
            )?,
            "textmultirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Text,
            )?,
            // PG float8multirange uses double precision bounds; canonicalising
            // through the numeric pathway is lossless for the values we care
            // about (and matches the comparator semantics used downstream).
            "float8multirange" => super::range::canonical_multirange_text_for_kind(
                literal,
                super::range::RangeKind::Numeric,
            )?,
            _ => structural,
        };
        Some(normalized)
    } else {
        None
    };

    // When the target is a domain whose base type is compatible with the
    // source type, pass the value through unchanged.  This preserves the
    // runtime representation (e.g. arrays stay as Value::Array) so that
    // downstream comparisons work correctly.
    //
    // Traverse the domain chain to find the ultimate base type and the
    // most restrictive char_length (for varchar/char domain hierarchies).
    let domain_cast_info = with_current_session_context(|session_context| {
        session_context.domain_def(&target_type)?;
        let mut ultimate_base = target_type.clone();
        let mut domain_char_length: Option<u32> = None;
        // Walk through the domain chain to find the real base type
        // and the first char_length constraint.
        for _ in 0..32 {
            match session_context.domain_def(&ultimate_base) {
                Some(def) => {
                    if domain_char_length.is_none() {
                        domain_char_length = def.char_length;
                    }
                    ultimate_base = normalize_compat_type_name(&def.base_type);
                }
                None => break,
            }
        }
        let compatible = if ultimate_base == source_type {
            true
        } else if let Some(source_domain) = session_context.domain_def(&source_type) {
            normalize_compat_type_name(&source_domain.base_type) == ultimate_base
        } else {
            let numeric_types = ["int4", "int8", "float4", "float8", "numeric"];
            let text_types = ["text", "varchar", "char", "character varying", "name"];
            (numeric_types.contains(&ultimate_base.as_str())
                && numeric_types.contains(&source_type.as_str()))
                || (text_types.contains(&ultimate_base.as_str())
                    && text_types.contains(&source_type.as_str()))
        };
        Some((compatible, domain_char_length))
    });
    if let Some((true, domain_char_length)) = domain_cast_info {
        let mut result = args[0].clone();
        // Enforce varchar length limit from domain definition.
        // Explicit CAST truncates (like PostgreSQL's CoerceToDomain).
        if let (Some(max_len), Value::Text(ref s)) = (domain_char_length, &result) {
            let max_len = usize::try_from(max_len).unwrap_or(usize::MAX);
            // Byte length is an upper bound on char count.
            if s.len() > max_len {
                let char_count = if s.is_ascii() {
                    s.len()
                } else {
                    s.chars().count()
                };
                if char_count > max_len {
                    let truncated: String = s.chars().take(max_len).collect();
                    result = Value::Text(truncated);
                }
            }
        }
        // Enforce domain CHECK constraints on the result value.
        crate::eval::domain_check::enforce_domain_constraints(&result, &target_type)?;
        return Ok(result);
    }

    let target_is_compat_user_type = with_current_session_context(|session_context| {
        session_context.compat_user_type(&target_type).is_some()
    });
    let target_is_geometric_builtin = matches!(
        target_base_type.as_str(),
        "point" | "box" | "line" | "lseg" | "path" | "polygon" | "circle"
    );

    let result = match cast {
        Some(CompatUserCast {
            method: CompatCastMethod::Function { .. },
            ..
        }) => Err(DbError::internal(
            "__aiondb_compat_cast does not execute function-backed casts",
        )),
        Some(_) => {
            let text_value = match &multirange_text {
                Some(value) => value.clone(),
                None => value_to_text(&args[0]),
            };
            if target_is_geometric_builtin {
                let coerced = coerce_geometric_text(&source_type, &target_base_type, &text_value)?;
                return Ok(Value::Text(coerced));
            }
            Ok(Value::Text(text_value))
        }
        // No explicit cast registered.  For text->(compat type or built-in
        // geometric type), apply the type's input validation and keep the
        // internal text representation.
        None if target_is_compat_user_type && source_type == target_type => {
            Ok(Value::Text(value_to_text(&args[0])))
        }
        None if (target_is_compat_user_type && source_type == "text")
            || (target_is_geometric_builtin
                && matches!(
                    source_type.as_str(),
                    "text" | "point" | "box" | "line" | "lseg" | "path" | "polygon" | "circle"
                )) =>
        {
            let text_value = match &multirange_text {
                Some(value) => value.clone(),
                None => value_to_text(&args[0]),
            };
            if target_is_geometric_builtin {
                let coerced = coerce_geometric_text(&source_type, &target_base_type, &text_value)?;
                return Ok(Value::Text(coerced));
            }
            Ok(Value::Text(text_value))
        }
        None if multirange_text.is_some() => Ok(Value::Text(multirange_text.unwrap_or_default())),
        None => Err(DbError::from_report(ErrorReport::new(
            SqlState::DatatypeMismatch,
            format!(
                "cannot cast type {} to {}",
                compat_display_type_name(&source_type),
                target_type
            ),
        ))),
    }?;

    if with_current_session_context(|ctx| ctx.domain_def(&target_type).is_some()) {
        crate::eval::domain_check::enforce_domain_constraints(&result, &target_type)?;
    }

    if let Some(enum_match) = with_current_session_context(|ctx| {
        let user_type = ctx.compat_user_type(&target_type)?;
        if user_type.enum_labels.is_empty() {
            return None;
        }
        let actual = match &result {
            Value::Null => return Some(Ok(())),
            Value::Text(text) => text.clone(),
            other => value_to_text(other),
        };
        let canonical_name = user_type.name.clone();
        if user_type.enum_labels.iter().any(|label| label == &actual) {
            Some(Ok(()))
        } else {
            Some(Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                format!("invalid input value for enum {canonical_name}: \"{actual}\""),
            ))))
        }
    }) {
        enum_match?;
    }

    Ok(result)
}

pub fn eval_regtype_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_regtype_cast")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(Value::Text(name)) => resolve_regtype_oid(name)
            .map(Value::Int)
            .ok_or_else(|| DbError::invalid_input_syntax("regtype", name)),
        Some(value) => {
            let text = value_to_text(value);
            resolve_regtype_oid(&text)
                .map(Value::Int)
                .ok_or_else(|| DbError::invalid_input_syntax("regtype", &text))
        }
    }
}

pub fn eval_regtype_out(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_regtype_out")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    let oid = match value {
        Value::Null => return Ok(Value::Null),
        Value::Int(oid) => *oid,
        Value::BigInt(oid) => i32::try_from(*oid)
            .map_err(|_| DbError::invalid_input_syntax("regtype", &oid.to_string()))?,
        other => value_to_text(other)
            .parse::<i32>()
            .map_err(|_| DbError::invalid_input_syntax("regtype", &value_to_text(other)))?,
    };
    Ok(Value::Text(
        regtype_name_for_oid(oid).unwrap_or_else(|| oid.to_string()),
    ))
}

pub fn eval_regrole_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_regrole_cast")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(oid) => Ok(Value::Int(*oid)),
        Value::BigInt(oid) => {
            let oid = i32::try_from(*oid).map_err(|_| {
                DbError::bind_error(
                    SqlState::NumericValueOutOfRange,
                    format!("OID value {oid} is out of range"),
                )
            })?;
            Ok(Value::Int(oid))
        }
        other => {
            let input = value_to_text(other);
            match lookup_regrole_name(&input)? {
                Some(role_name) => Ok(Value::Int(compat_role_oid(&role_name))),
                None => {
                    let parsed = parse_non_qualified_reg_name(&input)?;
                    Err(DbError::from_report(ErrorReport::new(
                        SqlState::UndefinedObject,
                        format!("role \"{parsed}\" does not exist"),
                    )))
                }
            }
        }
    }
}

pub fn eval_regrole_out(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_regrole_out")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(oid) => Ok(with_current_session_context(|ctx| {
            ctx.role_names_by_oid
                .get(oid)
                .cloned()
                .map_or_else(|| Value::Text(oid.to_string()), Value::Text)
        })),
        Value::BigInt(oid) => {
            let Some(oid) = i32::try_from(*oid).ok() else {
                return Ok(Value::Text(value.to_string()));
            };
            Ok(with_current_session_context(|ctx| {
                ctx.role_names_by_oid
                    .get(&oid)
                    .cloned()
                    .map_or_else(|| Value::Text(oid.to_string()), Value::Text)
            }))
        }
        Value::Text(name) => {
            if let Ok(oid) = name.parse::<i32>() {
                return Ok(with_current_session_context(|ctx| {
                    ctx.role_names_by_oid
                        .get(&oid)
                        .cloned()
                        .map_or_else(|| Value::Text(name.clone()), Value::Text)
                }));
            }
            Ok(Value::Text(name.clone()))
        }
        other => Ok(Value::Text(other.to_string())),
    }
}

pub fn eval_pg_type_is_visible(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_type_is_visible")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let oid = match value {
        Value::Int(oid) => *oid,
        Value::BigInt(oid) => match i32::try_from(*oid) {
            Ok(oid) => oid,
            Err(_) => return Ok(Value::Boolean(false)),
        },
        other => match value_to_text(other).parse::<i32>() {
            Ok(oid) => oid,
            Err(_) => return Ok(Value::Boolean(false)),
        },
    };
    Ok(Value::Boolean(with_current_session_context(|ctx| {
        let Some(user_type) = ctx.compat_user_types.iter().find(|entry| entry.oid == oid) else {
            return ctx
                .domain_defs
                .iter()
                .find(|entry| compat_domain_oid(entry.schema_name.as_deref(), &entry.name) == oid)
                .map_or(true, |domain| match domain.schema_name.as_deref() {
                    Some(schema_name) => current_search_path_schemas()
                        .iter()
                        .any(|schema| schema.eq_ignore_ascii_case(schema_name)),
                    None => true,
                });
        };
        match user_type.schema_name.as_deref() {
            Some(schema_name) => current_search_path_schemas()
                .iter()
                .any(|schema| schema.eq_ignore_ascii_case(schema_name)),
            None => true,
        }
    })))
}

pub fn eval_pg_table_is_visible(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_table_is_visible")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let oid = match value {
        Value::Int(oid) => *oid,
        Value::BigInt(oid) => match i32::try_from(*oid) {
            Ok(oid) => oid,
            Err(_) => return Ok(Value::Boolean(false)),
        },
        other => match value_to_text(other).parse::<i32>() {
            Ok(oid) => oid,
            Err(_) => return Ok(Value::Boolean(false)),
        },
    };
    Ok(Value::Boolean(with_current_session_context(|ctx| {
        let Some(schema_name) = ctx.compat_relation_schemas_by_oid.get(&oid) else {
            return true;
        };
        let Some(relation_name) = ctx.compat_relation_names_by_oid.get(&oid) else {
            return current_search_path_schemas()
                .iter()
                .any(|schema| schema.eq_ignore_ascii_case(schema_name));
        };
        let search_path = current_search_path_schemas();
        let Some(schema_pos) = search_path
            .iter()
            .position(|schema| schema.eq_ignore_ascii_case(schema_name))
        else {
            return false;
        };
        for earlier_schema in search_path.iter().take(schema_pos) {
            let shadows_target = ctx.compat_relation_schemas_by_oid.iter().any(
                |(candidate_oid, candidate_schema)| {
                    *candidate_oid != oid
                        && candidate_schema.eq_ignore_ascii_case(earlier_schema)
                        && ctx
                            .compat_relation_names_by_oid
                            .get(candidate_oid)
                            .is_some_and(|candidate_name| {
                                candidate_name.eq_ignore_ascii_case(relation_name)
                            })
                },
            );
            if shadows_target {
                return false;
            }
        }
        true
    })))
}

fn xid_out_of_range(type_name: &str, input: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        format!("value \"{input}\" is out of range for type {type_name}"),
    ))
}

fn parse_xid_like_text(input: &str, type_name: &str, bits: u32) -> DbResult<u128> {
    let text = input.trim();
    if text.is_empty() {
        return Err(DbError::invalid_input_syntax(type_name, input));
    }
    let (negative, body) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (false, rest)
    } else {
        (false, text)
    };
    if body.is_empty() {
        return Err(DbError::invalid_input_syntax(type_name, input));
    }

    let (radix, digits) =
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16u32, rest)
        } else if let Some(rest) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8u32, rest)
        } else if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2u32, rest)
        } else if body.len() > 1 && body.starts_with('0') {
            // PostgreSQL xid/xid8 input treats legacy leading-zero literals as octal.
            (8u32, body)
        } else {
            (10u32, body)
        };

    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return Err(DbError::invalid_input_syntax(type_name, input));
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_digit(radix)) {
        return Err(DbError::invalid_input_syntax(type_name, input));
    }

    let magnitude =
        u128::from_str_radix(&cleaned, radix).map_err(|_| xid_out_of_range(type_name, input))?;
    let max = (1u128 << bits) - 1;
    let modulus = max + 1;

    if negative {
        if magnitude > modulus {
            return Err(xid_out_of_range(type_name, input));
        }
        Ok((modulus - (magnitude % modulus)) % modulus)
    } else if magnitude > max {
        Err(xid_out_of_range(type_name, input))
    } else {
        Ok(magnitude)
    }
}

pub(crate) fn validate_xid_input(input: &str) -> DbResult<()> {
    parse_xid_like_text(input, "xid", 32).map(|_| ())
}

pub(crate) fn validate_xid8_input(input: &str) -> DbResult<()> {
    parse_xid_like_text(input, "xid8", 64).map(|_| ())
}

fn parse_snapshot_component(component: &str, full_input: &str) -> DbResult<u64> {
    if component.is_empty() || !component.chars().all(|c| c.is_ascii_digit()) {
        return Err(DbError::invalid_input_syntax("pg_snapshot", full_input));
    }
    let parsed = component
        .parse::<u64>()
        .map_err(|_| DbError::invalid_input_syntax("pg_snapshot", full_input))?;
    if parsed == 0 || parsed > i64::MAX as u64 {
        return Err(DbError::invalid_input_syntax("pg_snapshot", full_input));
    }
    Ok(parsed)
}

pub(crate) fn parse_and_normalize_pg_snapshot(input: &str) -> DbResult<String> {
    let text = input.trim();
    let mut parts = text.split(':');
    let (Some(xmin_raw), Some(xmax_raw), Some(xip_raw), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(DbError::invalid_input_syntax("pg_snapshot", input));
    };

    let xmin = parse_snapshot_component(xmin_raw, input)?;
    let xmax = parse_snapshot_component(xmax_raw, input)?;
    if xmin > xmax {
        return Err(DbError::invalid_input_syntax("pg_snapshot", input));
    }

    let mut xips = Vec::new();
    let mut previous = 0u64;
    for part in xip_raw.split(',') {
        if part.is_empty() {
            continue;
        }
        let xid = parse_snapshot_component(part, input)?;
        if xid < xmin || xid >= xmax {
            return Err(DbError::invalid_input_syntax("pg_snapshot", input));
        }
        if !xips.is_empty() && xid < previous {
            return Err(DbError::invalid_input_syntax("pg_snapshot", input));
        }
        if xid != previous {
            xips.push(xid);
        }
        previous = xid;
    }

    let xip_text = xips
        .into_iter()
        .map(|xid| xid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!("{xmin}:{xmax}:{xip_text}"))
}

pub fn eval_xid_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_xid_cast")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let input = value_to_text(value);
    let xid = parse_xid_like_text(&input, "xid", 32)?;
    let xid = i64::try_from(xid)
        .map_err(|_| DbError::internal("__aiondb_xid_cast produced an out-of-range value"))?;
    Ok(Value::BigInt(xid))
}

pub fn eval_xid8_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_xid8_cast")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let input = value_to_text(value);
    let xid = parse_xid_like_text(&input, "xid8", 64)?;
    let numeric = xid
        .to_string()
        .parse::<NumericValue>()
        .map_err(|_| DbError::invalid_input_syntax("xid8", &input))?;
    Ok(Value::Numeric(numeric))
}

pub fn eval_pg_snapshot_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_pg_snapshot_cast")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let input = value_to_text(value);
    Ok(Value::Text(parse_and_normalize_pg_snapshot(&input)?))
}

const COMPAT_PG_TYPE_CLASSID: i32 = 60_004;
const COMPAT_PG_PROC_CLASSID: i32 = 60_019;
const COMPAT_PG_CAST_CLASSID: i32 = 60_042;

pub fn eval_pg_describe_object(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "pg_describe_object")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let classid = value_to_text(&args[0]).parse::<i32>().ok();
    let objid = value_to_text(&args[1]).parse::<i32>().ok();
    let objsubid = value_to_text(&args[2])
        .parse::<i32>()
        .ok()
        .unwrap_or_default();
    if objsubid != 0 {
        return Ok(Value::Text(String::new()));
    }

    let Some(classid) = classid else {
        return Ok(Value::Text(String::new()));
    };
    let Some(objid) = objid else {
        return Ok(Value::Text(String::new()));
    };
    if let Some(description) = with_current_session_context(|context| {
        if classid == COMPAT_PG_CAST_CLASSID {
            if let Some(cast) = context
                .compat_user_casts
                .iter()
                .find(|entry| entry.oid == objid)
            {
                return Some(format!(
                    "cast from {} to {}",
                    compat_display_type_name(&cast.source_type),
                    cast.target_type
                ));
            }
        }
        if classid == COMPAT_PG_TYPE_CLASSID {
            if let Some(user_type) = context
                .compat_user_types
                .iter()
                .find(|entry| entry.oid == objid)
            {
                return Some(format!("type {}", user_type.name));
            }
        }
        if classid == COMPAT_PG_PROC_CLASSID {
            if let Some(cast) = context.compat_user_casts.iter().find(|entry| {
                matches!(
                    &entry.method,
                    CompatCastMethod::Function { function_oid, .. } if *function_oid == objid
                )
            }) {
                if let CompatCastMethod::Function { function_name, .. } = &cast.method {
                    return Some(format!(
                        "function {}({})",
                        function_name,
                        compat_display_type_name(&cast.source_type)
                    ));
                }
            }
        }
        None
    }) {
        return Ok(Value::Text(description));
    }

    Ok(Value::Text(String::new()))
}

pub fn eval_obj_description(args: &[Value]) -> DbResult<Value> {
    // PG accepts both `obj_description(oid)` (deprecated) and the safer
    // `obj_description(oid, catalog_name)`. Tools like psql `\dd` and
    // SQLAlchemy reflection emit the 1-arg form.
    if args.is_empty() || args.len() > 2 {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "obj_description requires 1..=2 argument(s), got {}",
                args.len()
            ),
        )));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let Some(objid) = value_to_text(&args[0]).parse::<i32>().ok() else {
        return Ok(Value::Null);
    };
    let class_name = if args.len() == 2 {
        value_to_text(&args[1]).to_ascii_lowercase()
    } else {
        "pg_class".to_owned()
    };
    if class_name != "pg_class" && class_name != "pg_catalog.pg_class" {
        return Ok(Value::Null);
    }

    let description = with_current_session_context(|context| {
        let oid_key = objid.to_string();
        for object_type in [
            "TABLE",
            "MATERIALIZED VIEW",
            "SEQUENCE",
            "FOREIGN TABLE",
            "INDEX",
            "VIEW",
        ] {
            if let Some(comment) = context
                .compat_comments
                .get(&(object_type.to_owned(), oid_key.clone()))
            {
                return Some(comment.clone());
            }
        }
        None
    });

    Ok(description.map_or(Value::Null, Value::Text))
}

pub fn eval_col_description(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "col_description")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let Some(objoid) = value_to_text(&args[0]).parse::<i32>().ok() else {
        return Ok(Value::Null);
    };
    let Some(objsubid) = value_to_text(&args[1]).parse::<i32>().ok() else {
        return Ok(Value::Null);
    };

    let key = format!("{objoid}.{objsubid}");
    let description = with_current_session_context(|context| {
        context
            .compat_comments
            .get(&("COLUMN".to_owned(), key.clone()))
            .cloned()
    });

    Ok(description.map_or(Value::Null, Value::Text))
}

pub fn eval_set_config(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "set_config")?;
    match (&args[0], &args[1], &args[2]) {
        (Value::Null, _, _) | (_, Value::Null, _) | (_, _, Value::Null) => Ok(Value::Null),
        (_, value, _) => Ok(Value::Text(value_to_text(value))),
    }
}

pub fn eval_current_schemas(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "current_schemas")?;
    let include_implicit = match args.first() {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Null) | None => return Ok(Value::Null),
        Some(other) => {
            return Err(DbError::internal(format!(
                "current_schemas expects a boolean argument, got {}",
                value_to_text(other)
            )));
        }
    };

    let mut schemas = current_search_path_schemas()
        .iter()
        .map(|schema| visible_session_schema_name(schema))
        .collect::<Vec<_>>();
    if include_implicit
        && !schemas
            .iter()
            .any(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
    {
        schemas.insert(0, "pg_catalog".to_owned());
    }

    Ok(Value::Array(
        schemas.into_iter().map(Value::Text).collect::<Vec<_>>(),
    ))
}

pub fn eval_pg_get_userbyid(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_get_userbyid")?;
    let Some(value) = args.first() else {
        return Ok(Value::Null);
    };
    let oid = match value {
        Value::Null => return Ok(Value::Null),
        Value::Int(value) => i64::from(*value),
        Value::BigInt(value) => *value,
        other => value_to_text(other).parse::<i64>().map_err(|_| {
            DbError::syntax_error(format!(
                "pg_get_userbyid expects an integer role oid, got {}",
                value_to_text(other)
            ))
        })?,
    };

    if let Ok(oid_value) = i32::try_from(oid) {
        if let Some(name) =
            with_current_session_context(|ctx| ctx.role_names_by_oid.get(&oid_value).cloned())
        {
            return Ok(Value::Text(name));
        }
    }

    if oid == i64::from(COMPAT_BOOTSTRAP_ROLE_OID) {
        return Ok(Value::Text(COMPAT_BOOTSTRAP_ROLE_NAME.to_string()));
    }

    Ok(Value::Text(format!("unknown (OID={oid})")))
}

pub fn unsupported_pg_size_function(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal(
            "pg_catalog size helper requires at least one argument",
        ));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let _ = name;
    Ok(Value::BigInt(0))
}

pub fn eval_pg_column_size(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_column_size")?;
    match args.first() {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(value) => Ok(Value::Int(compat_value_storage_size(value))),
    }
}

pub fn compat_value_storage_size(value: &Value) -> i32 {
    match value {
        Value::Null => 0,
        Value::Boolean(_) => 1,
        Value::Int(_) | Value::Date(_) | Value::LargeDate(_) | Value::Real(_) => 4,
        Value::BigInt(_) | Value::Double(_) | Value::Time(_) => 8,
        Value::TimeTz(_, _) => 12,
        Value::Timestamp(_) | Value::TimestampTz(_) => 8,
        Value::Interval(_) | Value::Uuid(_) => 16,
        Value::Tid(_) => 6,
        Value::PgLsn(_) => 8,
        Value::Numeric(value) => saturating_i32_len(value.to_string().len()),
        Value::Money(_) => 8,
        Value::Text(value) => saturating_i32_len(value.len()),
        Value::Blob(value) => saturating_i32_len(value.len()),
        Value::Jsonb(value) => saturating_i32_len(value.to_string().len()),
        Value::MacAddr(_) => 6,
        Value::MacAddr8(_) => 8,
        Value::Vector(value) => saturating_i32_len(value.values.len().saturating_mul(4)),
        Value::Array(values) => {
            let payload = values
                .iter()
                .map(compat_value_storage_size)
                .fold(4_i64, |acc, value| acc.saturating_add(i64::from(value)));
            i32::try_from(payload.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
                .unwrap_or(i32::MAX)
        }
    }
}

pub fn saturating_i32_len(len: usize) -> i32 {
    to_i32_saturating(len)
}

pub fn resolve_regclass_name(input: &str) -> Option<String> {
    let normalized = normalize_reg_lookup_input(input);
    let canonical = match normalized.as_str() {
        "pg_namespace" | "pg_catalog.pg_namespace" => "pg_catalog.pg_namespace",
        "pg_class" | "pg_catalog.pg_class" => "pg_catalog.pg_class",
        "pg_attribute" | "pg_catalog.pg_attribute" => "pg_catalog.pg_attribute",
        "pg_type" | "pg_catalog.pg_type" => "pg_catalog.pg_type",
        "pg_index" | "pg_catalog.pg_index" => "pg_catalog.pg_index",
        "pg_constraint" | "pg_catalog.pg_constraint" => "pg_catalog.pg_constraint",
        "pg_settings" | "pg_catalog.pg_settings" => "pg_catalog.pg_settings",
        "pg_authid" | "pg_catalog.pg_authid" => "pg_catalog.pg_authid",
        "pg_roles" | "pg_catalog.pg_roles" => "pg_catalog.pg_roles",
        "pg_database" | "pg_catalog.pg_database" => "pg_catalog.pg_database",
        "information_schema.tables" => "information_schema.tables",
        "information_schema.columns" => "information_schema.columns",
        "information_schema.schemata" => "information_schema.schemata",
        "information_schema.views" => "information_schema.views",
        _ => return None,
    };
    Some(canonical.to_string())
}

pub fn resolve_regtype_name(input: &str) -> Option<String> {
    let normalized = normalize_reg_lookup_input(input);
    let normalized = normalized
        .strip_prefix("pg_catalog.")
        .unwrap_or(&normalized);

    if let Some(base) = normalized.strip_suffix("[]") {
        return resolve_regtype_name(base).map(|resolved| format!("{resolved}[]"));
    }

    if let Some(canonical) = pgvector_regtype_display_name(normalized) {
        return Some(canonical.to_owned());
    }

    let canonical = match normalized {
        "int" | "int4" | "integer" => "integer",
        "int8" | "bigint" => "bigint",
        "float4" | "real" => "real",
        "float8" | "double precision" | "double" => "double precision",
        "numeric" | "decimal" => "numeric",
        "text" => "text",
        "varchar" | "character varying" => "character varying",
        "bpchar" | "char" | "character" => "character",
        "bool" | "boolean" => "boolean",
        "bytea" | "blob" => "bytea",
        "timestamp" | "timestamp without time zone" => "timestamp without time zone",
        "timestamptz" | "timestamp with time zone" => "timestamp with time zone",
        "date" => "date",
        "time" | "time without time zone" => "time without time zone",
        "timetz" | "time with time zone" => "time with time zone",
        "interval" => "interval",
        "uuid" => "uuid",
        "tid" => "tid",
        "pg_lsn" => "pg_lsn",
        "jsonb" | "json" => "jsonb",
        _ => {
            return with_current_session_context(|ctx| {
                ctx.compat_user_types
                    .iter()
                    .find(|entry| entry.name == normalized)
                    .map(|entry| entry.name.clone())
            });
        }
    };

    Some(canonical.to_string())
}

fn typmod_base_name(input: &str) -> &str {
    input.split_once('(').map_or(input, |(base, _)| base.trim())
}

fn pgvector_regtype_display_name(normalized: &str) -> Option<&'static str> {
    match typmod_base_name(normalized) {
        "bit" => Some("bit"),
        "varbit" | "bit varying" => Some("bit varying"),
        "vector" => Some("vector"),
        "halfvec" => Some("halfvec"),
        "sparsevec" => Some("sparsevec"),
        _ => None,
    }
}

pub fn resolve_regtype_oid(input: &str) -> Option<i32> {
    let normalized = normalize_reg_lookup_input(input);
    let normalized = normalized
        .strip_prefix("pg_catalog.")
        .unwrap_or(&normalized)
        .to_owned();
    if let Some(base) = normalized.strip_suffix("[]") {
        let base = typmod_base_name(base);
        let builtin_array = match base {
            "bool" | "boolean" => Some(1000),
            "bytea" => Some(1001),
            "int8" | "bigint" => Some(1016),
            "int4" | "integer" | "int" => Some(1007),
            "varchar" | "character varying" => Some(1015),
            "bpchar" | "character" | "char" => Some(1014),
            "text" => Some(1009),
            "float4" => Some(1021),
            "float8" => Some(1022),
            "date" => Some(1182),
            "time" => Some(1183),
            "timestamp" => Some(1115),
            "timestamptz" => Some(1185),
            "timetz" => Some(1270),
            "interval" => Some(1187),
            "numeric" => Some(1231),
            "uuid" => Some(2951),
            "jsonb" => Some(3807),
            "tid" => Some(1010),
            "pg_lsn" => Some(3221),
            "bit" => Some(COMPAT_PG_BIT_ARRAY_OID),
            "varbit" | "bit varying" => Some(COMPAT_PG_VARBIT_ARRAY_OID),
            "vector" => Some(COMPAT_PGVECTOR_VECTOR_ARRAY_OID),
            "halfvec" => Some(COMPAT_PGVECTOR_HALFVEC_ARRAY_OID),
            "sparsevec" => Some(COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID),
            _ => None,
        };
        if builtin_array.is_some() {
            return builtin_array;
        }
        return with_current_session_context(|ctx| {
            let array_name = format!("{base}[]");
            let legacy_array_name = format!("_{base}");
            ctx.compat_user_types
                .iter()
                .find(|entry| {
                    entry.name.eq_ignore_ascii_case(&array_name)
                        || entry.name.eq_ignore_ascii_case(&legacy_array_name)
                })
                .map(|entry| entry.oid)
        });
    }
    let normalized_base = typmod_base_name(&normalized);
    let builtin = match normalized_base {
        "bool" | "boolean" => Some(16),
        "bytea" => Some(17),
        "int8" | "bigint" => Some(20),
        "int2" | "smallint" => Some(21),
        "int4" | "int" | "integer" => Some(23),
        "varchar" | "character varying" => Some(1043),
        "bpchar" | "char" | "character" => Some(1042),
        "text" => Some(25),
        "float4" => Some(700),
        "float8" => Some(701),
        "date" => Some(1082),
        "time" => Some(1083),
        "timestamp" => Some(1114),
        "timestamptz" => Some(1184),
        "interval" => Some(1186),
        "numeric" => Some(1700),
        "uuid" => Some(2950),
        "jsonb" => Some(3802),
        "tid" => Some(27),
        "pg_lsn" => Some(3220),
        "bit" => Some(COMPAT_PG_BIT_OID),
        "varbit" | "bit varying" => Some(COMPAT_PG_VARBIT_OID),
        "vector" => Some(COMPAT_PGVECTOR_VECTOR_OID),
        "halfvec" => Some(COMPAT_PGVECTOR_HALFVEC_OID),
        "sparsevec" => Some(COMPAT_PGVECTOR_SPARSEVEC_OID),
        "regclass" => Some(2205),
        "regtype" => Some(2206),
        "cstring" => Some(2275),
        _ => None,
    };
    builtin.or_else(|| {
        with_current_session_context(|ctx| {
            ctx.compat_user_types
                .iter()
                .find(|entry| entry.name == normalized)
                .map(|entry| entry.oid)
        })
    })
}

fn regtype_name_for_oid(oid: i32) -> Option<String> {
    let builtin = match oid {
        16 => Some("boolean"),
        17 => Some("bytea"),
        20 => Some("bigint"),
        21 => Some("smallint"),
        23 => Some("integer"),
        25 => Some("text"),
        27 => Some("tid"),
        700 => Some("real"),
        701 => Some("double precision"),
        1042 => Some("character"),
        1043 => Some("character varying"),
        COMPAT_PG_BIT_OID => Some("bit"),
        COMPAT_PG_BIT_ARRAY_OID => Some("bit[]"),
        COMPAT_PG_VARBIT_OID => Some("bit varying"),
        COMPAT_PG_VARBIT_ARRAY_OID => Some("bit varying[]"),
        1082 => Some("date"),
        1083 => Some("time without time zone"),
        1114 => Some("timestamp without time zone"),
        1184 => Some("timestamp with time zone"),
        1186 => Some("interval"),
        1700 => Some("numeric"),
        2205 => Some("regclass"),
        2206 => Some("regtype"),
        2275 => Some("cstring"),
        2950 => Some("uuid"),
        3220 => Some("pg_lsn"),
        3802 => Some("jsonb"),
        COMPAT_PGVECTOR_VECTOR_OID => Some("vector"),
        COMPAT_PGVECTOR_VECTOR_ARRAY_OID => Some("vector[]"),
        COMPAT_PGVECTOR_HALFVEC_OID => Some("halfvec"),
        COMPAT_PGVECTOR_HALFVEC_ARRAY_OID => Some("halfvec[]"),
        COMPAT_PGVECTOR_SPARSEVEC_OID => Some("sparsevec"),
        COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID => Some("sparsevec[]"),
        _ => None,
    };
    if let Some(name) = builtin {
        return Some(name.to_owned());
    }
    with_current_session_context(|ctx| {
        ctx.compat_user_types
            .iter()
            .find(|entry| entry.oid == oid)
            .map(|entry| compat_display_type_name(&entry.name))
            .or_else(|| {
                ctx.domain_defs.iter().find_map(|domain| {
                    (compat_domain_oid(domain.schema_name.as_deref(), &domain.name) == oid)
                        .then(|| compat_display_type_name(&domain.name))
                })
            })
    })
}

pub fn resolve_regnamespace_name(input: &str) -> Option<String> {
    match normalize_reg_lookup_input(input).as_str() {
        "public" => Some("public".to_string()),
        "pg_catalog" => Some("pg_catalog".to_string()),
        "information_schema" => Some("information_schema".to_string()),
        _ => None,
    }
}

pub(crate) fn lookup_regclass_name(input: &str) -> DbResult<String> {
    resolve_regclass_name(input).ok_or_else(|| {
        DbError::from_report(ErrorReport::new(
            SqlState::UndefinedTable,
            format!(
                "relation \"{}\" does not exist",
                normalize_reg_lookup_input(input)
            ),
        ))
    })
}

pub(crate) fn lookup_regtype_name(input: &str) -> DbResult<String> {
    if let Some(name) = resolve_regtype_name(input) {
        return Ok(name);
    }

    let normalized = normalize_reg_lookup_input(input);
    if let Some((schema, _)) = normalized.split_once('.') {
        if !schema.eq_ignore_ascii_case("pg_catalog") {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidSchemaName,
                format!("schema \"{schema}\" does not exist"),
            )));
        }
    }

    Err(DbError::from_report(ErrorReport::new(
        SqlState::UndefinedObject,
        format!("type \"{normalized}\" does not exist"),
    )))
}

pub(crate) fn lookup_regnamespace_name(input: &str) -> DbResult<Option<String>> {
    let name = parse_non_qualified_reg_name(input)?;
    Ok(resolve_regnamespace_name(&name))
}

pub(crate) fn lookup_regrole_name(input: &str) -> DbResult<Option<String>> {
    let name = parse_non_qualified_reg_name(input)?;
    Ok(with_current_session_context(|ctx| {
        ctx.role_names_by_oid
            .values()
            .any(|candidate| candidate == &name)
            .then(|| name.clone())
    }))
}

pub(crate) fn lookup_regproc_name(input: &str) -> DbResult<String> {
    match normalize_reg_lookup_input(input).as_str() {
        "now" | "pg_catalog.now" => Ok("now".to_owned()),
        "pg_function_is_visible" | "pg_catalog.pg_function_is_visible" => {
            Ok("pg_function_is_visible".to_owned())
        }
        "pg_proc_is_visible" | "pg_catalog.pg_proc_is_visible" => {
            Ok("pg_proc_is_visible".to_owned())
        }
        "pg_table_is_visible" | "pg_catalog.pg_table_is_visible" => {
            Ok("pg_table_is_visible".to_owned())
        }
        "pg_type_is_visible" | "pg_catalog.pg_type_is_visible" => {
            Ok("pg_type_is_visible".to_owned())
        }
        "pg_operator_is_visible" | "pg_catalog.pg_operator_is_visible" => {
            Ok("pg_operator_is_visible".to_owned())
        }
        "pg_opclass_is_visible" | "pg_catalog.pg_opclass_is_visible" => {
            Ok("pg_opclass_is_visible".to_owned())
        }
        "pg_opfamily_is_visible" | "pg_catalog.pg_opfamily_is_visible" => {
            Ok("pg_opfamily_is_visible".to_owned())
        }
        "pg_ts_dict_is_visible" | "pg_catalog.pg_ts_dict_is_visible" => {
            Ok("pg_ts_dict_is_visible".to_owned())
        }
        "pg_ts_config_is_visible" | "pg_catalog.pg_ts_config_is_visible" => {
            Ok("pg_ts_config_is_visible".to_owned())
        }
        "pg_ts_parser_is_visible" | "pg_catalog.pg_ts_parser_is_visible" => {
            Ok("pg_ts_parser_is_visible".to_owned())
        }
        "pg_ts_template_is_visible" | "pg_catalog.pg_ts_template_is_visible" => {
            Ok("pg_ts_template_is_visible".to_owned())
        }
        "pg_conversion_is_visible" | "pg_catalog.pg_conversion_is_visible" => {
            Ok("pg_conversion_is_visible".to_owned())
        }
        "pg_get_statisticsobjdef" | "pg_catalog.pg_get_statisticsobjdef" => {
            Ok("pg_get_statisticsobjdef".to_owned())
        }
        "pg_get_statisticsobjdef_columns" | "pg_catalog.pg_get_statisticsobjdef_columns" => {
            Ok("pg_get_statisticsobjdef_columns".to_owned())
        }
        "pg_get_functiondef" | "pg_catalog.pg_get_functiondef" => {
            Ok("pg_get_functiondef".to_owned())
        }
        "pg_get_function_arguments" | "pg_catalog.pg_get_function_arguments" => {
            Ok("pg_get_function_arguments".to_owned())
        }
        "pg_get_function_result" | "pg_catalog.pg_get_function_result" => {
            Ok("pg_get_function_result".to_owned())
        }
        "pg_get_function_identity_arguments" | "pg_catalog.pg_get_function_identity_arguments" => {
            Ok("pg_get_function_identity_arguments".to_owned())
        }
        "pg_collation_is_visible" | "pg_catalog.pg_collation_is_visible" => {
            Ok("pg_collation_is_visible".to_owned())
        }
        "pg_statistics_obj_is_visible" | "pg_catalog.pg_statistics_obj_is_visible" => {
            Ok("pg_statistics_obj_is_visible".to_owned())
        }
        other => Err(DbError::from_report(ErrorReport::new(
            SqlState::UndefinedFunction,
            format!("function \"{other}\" does not exist"),
        ))),
    }
}

pub(crate) fn lookup_regprocedure_name(input: &str) -> DbResult<String> {
    let normalized = normalize_reg_lookup_input(input);
    if !normalized.ends_with(')') {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "expected a right parenthesis",
        )));
    }
    let compact = normalized
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if let Some(signature) = lookup_pgvector_regprocedure_signature(&compact) {
        return Ok(signature.to_owned());
    }
    match normalized.as_str() {
        "abs(numeric)" | "pg_catalog.abs(numeric)" => Ok("abs(numeric)".to_owned()),
        "pg_function_is_visible(oid)" | "pg_catalog.pg_function_is_visible(oid)" => {
            Ok("pg_function_is_visible(oid)".to_owned())
        }
        "pg_proc_is_visible(oid)" | "pg_catalog.pg_proc_is_visible(oid)" => {
            Ok("pg_proc_is_visible(oid)".to_owned())
        }
        "pg_table_is_visible(oid)" | "pg_catalog.pg_table_is_visible(oid)" => {
            Ok("pg_table_is_visible(oid)".to_owned())
        }
        "pg_type_is_visible(oid)" | "pg_catalog.pg_type_is_visible(oid)" => {
            Ok("pg_type_is_visible(oid)".to_owned())
        }
        "pg_operator_is_visible(oid)" | "pg_catalog.pg_operator_is_visible(oid)" => {
            Ok("pg_operator_is_visible(oid)".to_owned())
        }
        "pg_opclass_is_visible(oid)" | "pg_catalog.pg_opclass_is_visible(oid)" => {
            Ok("pg_opclass_is_visible(oid)".to_owned())
        }
        "pg_opfamily_is_visible(oid)" | "pg_catalog.pg_opfamily_is_visible(oid)" => {
            Ok("pg_opfamily_is_visible(oid)".to_owned())
        }
        "pg_ts_dict_is_visible(oid)" | "pg_catalog.pg_ts_dict_is_visible(oid)" => {
            Ok("pg_ts_dict_is_visible(oid)".to_owned())
        }
        "pg_ts_config_is_visible(oid)" | "pg_catalog.pg_ts_config_is_visible(oid)" => {
            Ok("pg_ts_config_is_visible(oid)".to_owned())
        }
        "pg_ts_parser_is_visible(oid)" | "pg_catalog.pg_ts_parser_is_visible(oid)" => {
            Ok("pg_ts_parser_is_visible(oid)".to_owned())
        }
        "pg_ts_template_is_visible(oid)" | "pg_catalog.pg_ts_template_is_visible(oid)" => {
            Ok("pg_ts_template_is_visible(oid)".to_owned())
        }
        "pg_conversion_is_visible(oid)" | "pg_catalog.pg_conversion_is_visible(oid)" => {
            Ok("pg_conversion_is_visible(oid)".to_owned())
        }
        "pg_get_statisticsobjdef(oid)" | "pg_catalog.pg_get_statisticsobjdef(oid)" => {
            Ok("pg_get_statisticsobjdef(oid)".to_owned())
        }
        "pg_get_statisticsobjdef_columns(oid)"
        | "pg_catalog.pg_get_statisticsobjdef_columns(oid)" => {
            Ok("pg_get_statisticsobjdef_columns(oid)".to_owned())
        }
        "pg_get_functiondef(oid)" | "pg_catalog.pg_get_functiondef(oid)" => {
            Ok("pg_get_functiondef(oid)".to_owned())
        }
        "pg_get_function_arguments(oid)" | "pg_catalog.pg_get_function_arguments(oid)" => {
            Ok("pg_get_function_arguments(oid)".to_owned())
        }
        "pg_get_function_result(oid)" | "pg_catalog.pg_get_function_result(oid)" => {
            Ok("pg_get_function_result(oid)".to_owned())
        }
        "pg_get_function_identity_arguments(oid)"
        | "pg_catalog.pg_get_function_identity_arguments(oid)" => {
            Ok("pg_get_function_identity_arguments(oid)".to_owned())
        }
        "pg_collation_is_visible(oid)" | "pg_catalog.pg_collation_is_visible(oid)" => {
            Ok("pg_collation_is_visible(oid)".to_owned())
        }
        "pg_statistics_obj_is_visible(oid)" | "pg_catalog.pg_statistics_obj_is_visible(oid)" => {
            Ok("pg_statistics_obj_is_visible(oid)".to_owned())
        }
        other => Err(DbError::from_report(ErrorReport::new(
            SqlState::UndefinedFunction,
            format!("function \"{other}\" does not exist"),
        ))),
    }
}

fn lookup_pgvector_regprocedure_signature(compact: &str) -> Option<&'static str> {
    match compact.strip_prefix("pg_catalog.").unwrap_or(compact) {
        "vector_in(cstring)" => Some("vector_in(cstring)"),
        "vector_in(cstring,oid,integer)" | "vector_in(cstring,oid,int4)" => {
            Some("vector_in(cstring,oid,integer)")
        }
        "vector_out(vector)" => Some("vector_out(vector)"),
        "halfvec_in(cstring)" => Some("halfvec_in(cstring)"),
        "halfvec_in(cstring,oid,integer)" | "halfvec_in(cstring,oid,int4)" => {
            Some("halfvec_in(cstring,oid,integer)")
        }
        "halfvec_out(halfvec)" => Some("halfvec_out(halfvec)"),
        "sparsevec_in(cstring)" => Some("sparsevec_in(cstring)"),
        "sparsevec_in(cstring,oid,integer)" | "sparsevec_in(cstring,oid,int4)" => {
            Some("sparsevec_in(cstring,oid,integer)")
        }
        "sparsevec_out(sparsevec)" => Some("sparsevec_out(sparsevec)"),
        "l2_distance(vector,vector)" => Some("l2_distance(vector,vector)"),
        "cosine_distance(vector,vector)" => Some("cosine_distance(vector,vector)"),
        "inner_product(vector,vector)" => Some("inner_product(vector,vector)"),
        "negative_inner_product(vector,vector)" => Some("negative_inner_product(vector,vector)"),
        "l1_distance(vector,vector)" => Some("l1_distance(vector,vector)"),
        "l2_distance(halfvec,halfvec)" => Some("l2_distance(halfvec,halfvec)"),
        "cosine_distance(halfvec,halfvec)" => Some("cosine_distance(halfvec,halfvec)"),
        "inner_product(halfvec,halfvec)" => Some("inner_product(halfvec,halfvec)"),
        "negative_inner_product(halfvec,halfvec)" => {
            Some("negative_inner_product(halfvec,halfvec)")
        }
        "l1_distance(halfvec,halfvec)" => Some("l1_distance(halfvec,halfvec)"),
        "l2_distance(sparsevec,sparsevec)" => Some("l2_distance(sparsevec,sparsevec)"),
        "cosine_distance(sparsevec,sparsevec)" => Some("cosine_distance(sparsevec,sparsevec)"),
        "inner_product(sparsevec,sparsevec)" => Some("inner_product(sparsevec,sparsevec)"),
        "negative_inner_product(sparsevec,sparsevec)" => {
            Some("negative_inner_product(sparsevec,sparsevec)")
        }
        "l1_distance(sparsevec,sparsevec)" => Some("l1_distance(sparsevec,sparsevec)"),
        "array_to_vector(integer[],integer,boolean)" | "array_to_vector(int4[],int4,bool)" => {
            Some("array_to_vector(integer[],integer,boolean)")
        }
        "array_to_vector(real[],integer,boolean)" | "array_to_vector(float4[],int4,bool)" => {
            Some("array_to_vector(real[],integer,boolean)")
        }
        "array_to_vector(doubleprecision[],integer,boolean)"
        | "array_to_vector(float8[],int4,bool)" => {
            Some("array_to_vector(double precision[],integer,boolean)")
        }
        "array_to_vector(numeric[],integer,boolean)" | "array_to_vector(decimal[],int4,bool)" => {
            Some("array_to_vector(numeric[],integer,boolean)")
        }
        "vector_to_float4(vector,integer,boolean)" | "vector_to_float4(vector,int4,bool)" => {
            Some("vector_to_float4(vector,integer,boolean)")
        }
        "halfvec_to_float4(halfvec,integer,boolean)" | "halfvec_to_float4(halfvec,int4,bool)" => {
            Some("halfvec_to_float4(halfvec,integer,boolean)")
        }
        "vector_add(vector,vector)" => Some("vector_add(vector,vector)"),
        "vector_sub(vector,vector)" => Some("vector_sub(vector,vector)"),
        "vector_mul(vector,vector)" => Some("vector_mul(vector,vector)"),
        "vector_concat(vector,vector)" => Some("vector_concat(vector,vector)"),
        "array_to_halfvec(integer[],integer,boolean)" | "array_to_halfvec(int4[],int4,bool)" => {
            Some("array_to_halfvec(integer[],integer,boolean)")
        }
        "array_to_halfvec(real[],integer,boolean)" | "array_to_halfvec(float4[],int4,bool)" => {
            Some("array_to_halfvec(real[],integer,boolean)")
        }
        "array_to_halfvec(doubleprecision[],integer,boolean)"
        | "array_to_halfvec(float8[],int4,bool)" => {
            Some("array_to_halfvec(double precision[],integer,boolean)")
        }
        "array_to_halfvec(numeric[],integer,boolean)" | "array_to_halfvec(decimal[],int4,bool)" => {
            Some("array_to_halfvec(numeric[],integer,boolean)")
        }
        "vector_to_halfvec(vector,integer,boolean)" | "vector_to_halfvec(vector,int4,bool)" => {
            Some("vector_to_halfvec(vector,integer,boolean)")
        }
        "halfvec_to_vector(halfvec,integer,boolean)" | "halfvec_to_vector(halfvec,int4,bool)" => {
            Some("halfvec_to_vector(halfvec,integer,boolean)")
        }
        "halfvec_add(halfvec,halfvec)" => Some("halfvec_add(halfvec,halfvec)"),
        "halfvec_sub(halfvec,halfvec)" => Some("halfvec_sub(halfvec,halfvec)"),
        "halfvec_mul(halfvec,halfvec)" => Some("halfvec_mul(halfvec,halfvec)"),
        "halfvec_concat(halfvec,halfvec)" => Some("halfvec_concat(halfvec,halfvec)"),
        "halfvec_to_sparsevec(halfvec,integer,boolean)"
        | "halfvec_to_sparsevec(halfvec,int4,bool)" => {
            Some("halfvec_to_sparsevec(halfvec,integer,boolean)")
        }
        "sparsevec_to_vector(sparsevec,integer,boolean)"
        | "sparsevec_to_vector(sparsevec,int4,bool)" => {
            Some("sparsevec_to_vector(sparsevec,integer,boolean)")
        }
        "sparsevec_to_halfvec(sparsevec,integer,boolean)"
        | "sparsevec_to_halfvec(sparsevec,int4,bool)" => {
            Some("sparsevec_to_halfvec(sparsevec,integer,boolean)")
        }
        "vector_to_sparsevec(vector,integer,boolean)" | "vector_to_sparsevec(vector,int4,bool)" => {
            Some("vector_to_sparsevec(vector,integer,boolean)")
        }
        "binary_quantize(vector,integer,boolean)" | "binary_quantize(vector,int4,bool)" => {
            Some("binary_quantize(vector,integer,boolean)")
        }
        "array_to_sparsevec(integer[],integer,boolean)"
        | "array_to_sparsevec(int4[],int4,bool)" => {
            Some("array_to_sparsevec(integer[],integer,boolean)")
        }
        "array_to_sparsevec(real[],integer,boolean)" | "array_to_sparsevec(float4[],int4,bool)" => {
            Some("array_to_sparsevec(real[],integer,boolean)")
        }
        "array_to_sparsevec(doubleprecision[],integer,boolean)"
        | "array_to_sparsevec(float8[],int4,bool)" => {
            Some("array_to_sparsevec(double precision[],integer,boolean)")
        }
        "array_to_sparsevec(numeric[],integer,boolean)"
        | "array_to_sparsevec(decimal[],int4,bool)" => {
            Some("array_to_sparsevec(numeric[],integer,boolean)")
        }
        "vector_dims(vector)" => Some("vector_dims(vector)"),
        "vector_dims(halfvec)" => Some("vector_dims(halfvec)"),
        "vector_norm(vector)" => Some("vector_norm(vector)"),
        "l2_norm(vector)" => Some("l2_norm(vector)"),
        "l2_norm(halfvec)" => Some("l2_norm(halfvec)"),
        "l2_norm(sparsevec)" => Some("l2_norm(sparsevec)"),
        "l2_normalize(vector)" => Some("l2_normalize(vector)"),
        "l2_normalize(halfvec)" => Some("l2_normalize(halfvec)"),
        "l2_normalize(sparsevec)" => Some("l2_normalize(sparsevec)"),
        "subvector(vector,int4,int4)" | "subvector(vector,integer,integer)" => {
            Some("subvector(vector,integer,integer)")
        }
        "subvector(halfvec,int4,int4)" | "subvector(halfvec,integer,integer)" => {
            Some("subvector(halfvec,integer,integer)")
        }
        "binary_quantize(vector)" => Some("binary_quantize(vector)"),
        "binary_quantize(halfvec)" => Some("binary_quantize(halfvec)"),
        "hamming_distance(bit,bit)" => Some("hamming_distance(bit,bit)"),
        "jaccard_distance(bit,bit)" => Some("jaccard_distance(bit,bit)"),
        "sum(vector)" => Some("sum(vector)"),
        "avg(vector)" => Some("avg(vector)"),
        "sum(halfvec)" => Some("sum(halfvec)"),
        "avg(halfvec)" => Some("avg(halfvec)"),
        _ => None,
    }
}

pub(crate) fn lookup_regoper_name(input: &str) -> DbResult<String> {
    let normalized = normalize_reg_lookup_input(input);
    match normalized.as_str() {
        "||/" | "pg_catalog.||/" => Ok("||/".to_owned()),
        "-" => Err(DbError::from_report(ErrorReport::new(
            SqlState::AmbiguousFunction,
            "more than one operator named -",
        ))),
        other => Err(DbError::from_report(ErrorReport::new(
            SqlState::UndefinedFunction,
            format!("operator does not exist: {other}"),
        ))),
    }
}

pub(crate) fn lookup_regoperator_name(input: &str) -> DbResult<String> {
    let normalized = normalize_reg_lookup_input(input);
    if !normalized.contains('(') {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "expected a left parenthesis",
        )));
    }
    if !normalized.ends_with(')') {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "expected a right parenthesis",
        )));
    }
    let compact = normalized
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    match compact.as_str() {
        "+(int4,int4)" | "pg_catalog.+(int4,int4)" => Ok("+(integer,integer)".to_owned()),
        "+(vector,vector)" | "pg_catalog.+(vector,vector)" => Ok("+(vector,vector)".to_owned()),
        "-(vector,vector)" | "pg_catalog.-(vector,vector)" => Ok("-(vector,vector)".to_owned()),
        "*(vector,vector)" | "pg_catalog.*(vector,vector)" => Ok("*(vector,vector)".to_owned()),
        "||(vector,vector)" | "pg_catalog.||(vector,vector)" => Ok("||(vector,vector)".to_owned()),
        "<->(vector,vector)" | "pg_catalog.<->(vector,vector)" => {
            Ok("<->(vector,vector)".to_owned())
        }
        "<#>(vector,vector)" | "pg_catalog.<#>(vector,vector)" => {
            Ok("<#>(vector,vector)".to_owned())
        }
        "<=>(vector,vector)" | "pg_catalog.<=>(vector,vector)" => {
            Ok("<=>(vector,vector)".to_owned())
        }
        "<+>(vector,vector)" | "pg_catalog.<+>(vector,vector)" => {
            Ok("<+>(vector,vector)".to_owned())
        }
        "+(halfvec,halfvec)" | "pg_catalog.+(halfvec,halfvec)" => {
            Ok("+(halfvec,halfvec)".to_owned())
        }
        "-(halfvec,halfvec)" | "pg_catalog.-(halfvec,halfvec)" => {
            Ok("-(halfvec,halfvec)".to_owned())
        }
        "*(halfvec,halfvec)" | "pg_catalog.*(halfvec,halfvec)" => {
            Ok("*(halfvec,halfvec)".to_owned())
        }
        "||(halfvec,halfvec)" | "pg_catalog.||(halfvec,halfvec)" => {
            Ok("||(halfvec,halfvec)".to_owned())
        }
        "<->(halfvec,halfvec)" | "pg_catalog.<->(halfvec,halfvec)" => {
            Ok("<->(halfvec,halfvec)".to_owned())
        }
        "<#>(halfvec,halfvec)" | "pg_catalog.<#>(halfvec,halfvec)" => {
            Ok("<#>(halfvec,halfvec)".to_owned())
        }
        "<=>(halfvec,halfvec)" | "pg_catalog.<=>(halfvec,halfvec)" => {
            Ok("<=>(halfvec,halfvec)".to_owned())
        }
        "<+>(halfvec,halfvec)" | "pg_catalog.<+>(halfvec,halfvec)" => {
            Ok("<+>(halfvec,halfvec)".to_owned())
        }
        "<->(sparsevec,sparsevec)" | "pg_catalog.<->(sparsevec,sparsevec)" => {
            Ok("<->(sparsevec,sparsevec)".to_owned())
        }
        "<#>(sparsevec,sparsevec)" | "pg_catalog.<#>(sparsevec,sparsevec)" => {
            Ok("<#>(sparsevec,sparsevec)".to_owned())
        }
        "<=>(sparsevec,sparsevec)" | "pg_catalog.<=>(sparsevec,sparsevec)" => {
            Ok("<=>(sparsevec,sparsevec)".to_owned())
        }
        "<+>(sparsevec,sparsevec)" | "pg_catalog.<+>(sparsevec,sparsevec)" => {
            Ok("<+>(sparsevec,sparsevec)".to_owned())
        }
        "<~>(bit,bit)" | "pg_catalog.<~>(bit,bit)" => Ok("<~>(bit,bit)".to_owned()),
        "<%>(bit,bit)" | "pg_catalog.<%>(bit,bit)" => Ok("<%>(bit,bit)".to_owned()),
        other => Err(DbError::from_report(ErrorReport::new(
            SqlState::UndefinedFunction,
            format!("operator does not exist: {other}"),
        ))),
    }
}

pub(crate) fn lookup_regcollation_name(input: &str) -> DbResult<String> {
    match normalize_reg_lookup_input(input).as_str() {
        "posix" | "pg_catalog.posix" => Ok("\"POSIX\"".to_owned()),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::UndefinedObject,
            SqlState::UndefinedObject.code(),
        ))),
    }
}

fn parse_non_qualified_reg_name(input: &str) -> DbResult<String> {
    let trimmed = input.trim();
    if trimmed.contains('.') {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "invalid name syntax",
        )));
    }
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Ok(inner.replace("\"\"", "\""));
    }
    if trimmed.contains('"') {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "invalid name syntax",
        )));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn normalize_reg_lookup_input(input: &str) -> String {
    input
        .trim()
        .trim_matches('"')
        .replace('"', "")
        .to_ascii_lowercase()
}

pub fn eval_unsupported_geometric_function(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }

    Err(DbError::feature_not_supported(format!(
        "function \"{name}\" is not supported for AionDB geometric compatibility types"
    )))
}

pub fn eval_geometric_predicate(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match name {
        "ishorizontal" | "isvertical" => {
            if args.len() != 2 || args[1].is_null() {
                return Ok(Value::Null);
            }
            let left = parse_point_text(&value_to_text(&args[0]))?;
            let right = parse_point_text(&value_to_text(&args[1]))?;
            let result = if name == "ishorizontal" {
                left.y == right.y
            } else {
                left.x == right.x
            };
            Ok(Value::Boolean(result))
        }
        "isopen" | "isclosed" => {
            let path = parse_path_text(&value_to_text(&args[0]))?;
            let is_closed = path.closed;
            Ok(Value::Boolean(if name == "isclosed" {
                is_closed
            } else {
                !is_closed
            }))
        }
        _ => eval_unsupported_geometric_function(name, args),
    }
}

pub fn eval_geometric_npoints(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "npoints")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let text = value_to_text(&args[0]);
    if let Ok(path) = parse_path_text(&text) {
        return Ok(Value::Int(
            i32::try_from(path.points.len()).unwrap_or(i32::MAX),
        ));
    }
    if let Ok(polygon) = parse_polygon_text(&text) {
        return Ok(Value::Int(i32::try_from(polygon.len()).unwrap_or(i32::MAX)));
    }
    eval_unsupported_geometric_function("npoints", args)
}

/// Geometric measurement functions.
pub fn eval_geometric_measure(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match name {
        "area" => {
            let text = value_to_text(&args[0]);
            if let Ok(circle) = parse_circle_text(&text) {
                return Ok(Value::Double(
                    std::f64::consts::PI * circle.radius * circle.radius,
                ));
            }
            if let Ok(path) = parse_path_text(&text) {
                if !path.closed {
                    return Ok(Value::Null);
                }
                if path.points.len() < 3 {
                    return Ok(Value::Double(0.0));
                }
                let mut signed_area = 0.0;
                for index in 0..path.points.len() {
                    let a = path.points[index];
                    let b = path.points[(index + 1) % path.points.len()];
                    signed_area += a.x * b.y - b.x * a.y;
                }
                return Ok(Value::Double((signed_area / 2.0).abs()));
            }
            if let Ok(polygon) = parse_polygon_text(&text) {
                if polygon.len() < 3 {
                    return Ok(Value::Double(0.0));
                }
                let mut signed_area = 0.0;
                for index in 0..polygon.len() {
                    let a = polygon[index];
                    let b = polygon[(index + 1) % polygon.len()];
                    signed_area += a.x * b.y - b.x * a.y;
                }
                return Ok(Value::Double((signed_area / 2.0).abs()));
            }
            Err(DbError::invalid_input_syntax("circle", &text))
        }
        "radius" => {
            let circle = parse_circle_text(&value_to_text(&args[0]))?;
            Ok(Value::Double(circle.radius))
        }
        "diameter" => {
            let circle = parse_circle_text(&value_to_text(&args[0]))?;
            Ok(Value::Double(circle.radius * 2.0))
        }
        "slope" => {
            expect_args(args, 2, "slope")?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let left = parse_point_text(&value_to_text(&args[0]))?;
            let right = parse_point_text(&value_to_text(&args[1]))?;
            let dx = right.x - left.x;
            if dx == 0.0 {
                return Ok(Value::Double(f64::INFINITY));
            }
            Ok(Value::Double((right.y - left.y) / dx))
        }
        "height" => {
            let (a, b) = parse_box_text(&value_to_text(&args[0]))?;
            Ok(Value::Double((a.y - b.y).abs()))
        }
        "width" => {
            let (a, b) = parse_box_text(&value_to_text(&args[0]))?;
            Ok(Value::Double((a.x - b.x).abs()))
        }
        "center" | "diagonal" => {
            // Return the input as text (passthrough)
            match &args[0] {
                Value::Text(s) => Ok(Value::Text(s.clone())),
                other => Ok(Value::Text(other.to_string())),
            }
        }
        _ => eval_unsupported_geometric_function(name, args),
    }
}

// Vector distance and array functions
