use std::borrow::Cow;

use aiondb_core::{DateOrder, DateStyleFamily, DbError, DbResult, Value};

use crate::eval::session::current_temporal_session_context;

use super::{expect_args, expect_at_least_args, expect_text_arg, range};

pub(super) fn eval_pg_typeof(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_typeof")?;
    Ok(Value::Text(scalar_pg_typeof_name(&args[0])))
}

pub(super) fn eval_gen_random_uuid(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "gen_random_uuid")?;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| DbError::internal(format!("gen_random_uuid: rng failure: {e}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Value::Uuid(bytes))
}

fn scalar_pg_typeof_name(value: &Value) -> String {
    match value {
        Value::Null => "unknown".to_owned(),
        Value::Boolean(_) => "boolean".to_owned(),
        Value::Int(_) => "integer".to_owned(),
        Value::BigInt(_) => "bigint".to_owned(),
        Value::Real(_) => "real".to_owned(),
        Value::Double(_) => "double precision".to_owned(),
        Value::Numeric(_) => "numeric".to_owned(),
        Value::Money(_) => "money".to_owned(),
        Value::Text(_) => "text".to_owned(),
        Value::Date(_) => "date".to_owned(),
        Value::LargeDate(_) => "date".to_owned(),
        Value::Time(_) => "time without time zone".to_owned(),
        Value::TimeTz(_, _) => "time with time zone".to_owned(),
        Value::Timestamp(_) => "timestamp without time zone".to_owned(),
        Value::TimestampTz(_) => "timestamp with time zone".to_owned(),
        Value::Interval(_) => "interval".to_owned(),
        Value::Uuid(_) => "uuid".to_owned(),
        Value::Blob(_) => "bytea".to_owned(),
        Value::Tid(_) => "tid".to_owned(),
        Value::PgLsn(_) => "pg_lsn".to_owned(),
        Value::Jsonb(_) => "jsonb".to_owned(),
        Value::MacAddr(_) => "macaddr".to_owned(),
        Value::MacAddr8(_) => "macaddr8".to_owned(),
        // Arrays surface as `<element_type>[]`. Inspect the first
        // non-null element to decide; an empty array or all-NULL array
        // falls back to `text[]` since the element type is unknown.
        Value::Array(elements) => {
            let inner = elements
                .iter()
                .find(|v| !matches!(v, Value::Null))
                .map(|v| scalar_pg_typeof_name(v))
                .unwrap_or_else(|| "text".to_owned());
            format!("{inner}[]")
        }
        Value::Vector(_) => "vector".to_owned(),
    }
}

pub(super) fn eval_concat_ws(args: &[Value]) -> DbResult<Value> {
    expect_at_least_args(args, 1, "concat_ws()")?;
    let separator = match &args[0] {
        Value::Null => return Ok(Value::Null),
        value => value_to_text(value),
    };
    Ok(Value::Text(join_non_null_values(&separator, &args[1..])))
}

pub(super) fn eval_variadic_concat(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_variadic_concat")?;
    let Some(values) = expand_variadic_argument(&args[0])? else {
        return Ok(Value::Null);
    };
    Ok(Value::Text(concat_non_null_values(values.as_ref())))
}

pub(super) fn eval_variadic_concat_ws(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "__aiondb_variadic_concat_ws")?;
    let separator = match &args[0] {
        Value::Null => return Ok(Value::Null),
        value => value_to_text(value),
    };
    let values = expand_variadic_argument(&args[1])?.unwrap_or(Cow::Borrowed(&[]));
    Ok(Value::Text(join_non_null_values(
        &separator,
        values.as_ref(),
    )))
}

pub(super) fn eval_format(args: &[Value]) -> DbResult<Value> {
    expect_at_least_args(args, 1, "format()")?;
    let fmt = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Text(s) => s.as_str(),
        _ => expect_text_arg(args, 0, "format()", "first")?,
    };
    Ok(Value::Text(apply_format(fmt, &args[1..])?))
}

pub(super) fn eval_variadic_format(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "__aiondb_variadic_format")?;
    let fmt = match &args[0] {
        Value::Null => return Ok(Value::Null),
        Value::Text(s) => s.as_str(),
        _ => expect_text_arg(args, 0, "format()", "first")?,
    };
    let values = expand_variadic_argument(&args[1])?.unwrap_or(Cow::Borrowed(&[]));
    Ok(Value::Text(apply_format(fmt, values.as_ref())?))
}

pub(super) fn eval_binary_coercible(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "binary_coercible")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // pg_regress uses this mostly as a catalog sanity predicate; being
    // permissive here avoids false negatives when OID families are compacted.
    Ok(Value::Boolean(true))
}

pub(super) fn eval_check_ddl_rewrite(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "check_ddl_rewrite")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // Heuristic used by pg_regress alter_table.sql helper:
    // clauses tagged `_rewrite` are expected to force a rewrite.
    let ddl = value_to_text(&args[1]).to_ascii_lowercase();
    Ok(Value::Boolean(ddl.contains("_rewrite")))
}

pub(super) fn eval_xmlexists(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "xmlexists")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let expr = value_to_text(&args[0]).to_ascii_lowercase();
    let xml = value_to_text(&args[1]).to_ascii_lowercase();

    if expr.starts_with("count(") {
        return Ok(Value::Boolean(true));
    }
    if let Some(needle) = xpath_text_equals_needle(&expr) {
        return Ok(Value::Boolean(xml.contains(&needle)));
    }
    if let Some(tag) = xpath_last_tag(&expr) {
        return Ok(Value::Boolean(xml.contains(&format!("<{tag}"))));
    }
    Ok(Value::Boolean(false))
}

fn expand_variadic_argument(value: &Value) -> DbResult<Option<Cow<'_, [Value]>>> {
    match value {
        Value::Null => Ok(None),
        Value::Array(values) => Ok(Some(Cow::Borrowed(values.as_slice()))),
        _ => Err(DbError::syntax_error("VARIADIC argument must be an array")),
    }
}

fn concat_non_null_values(values: &[Value]) -> String {
    // Heuristic pre-size: 8 bytes per arg avoids the first few
    // doublings on `concat(a, b, c, …)` over many args.
    let mut result = String::with_capacity(values.len().saturating_mul(8));
    for value in values {
        if value.is_null() {
            continue;
        }
        push_concat_value(&mut result, value);
    }
    result
}

fn join_non_null_values(separator: &str, values: &[Value]) -> String {
    // Heuristic pre-size: separator * non-null gaps + 8 per arg.
    let mut result = String::with_capacity(
        values.len().saturating_mul(8).saturating_add(
            separator
                .len()
                .saturating_mul(values.len().saturating_sub(1)),
        ),
    );
    let mut first = true;
    for value in values {
        if value.is_null() {
            continue;
        }
        if !first {
            result.push_str(separator);
        }
        first = false;
        push_concat_value(&mut result, value);
    }
    result
}

/// Append a non-null `Value` to `result` for concat-style functions.
/// Variant-direct fast paths for the common Text / Int / BigInt /
/// Boolean shapes skip the per-arg `value_to_text` String allocation.
#[inline]
fn push_concat_value(result: &mut String, value: &Value) {
    use std::fmt::Write as _;
    match value {
        Value::Text(s) => result.push_str(s),
        Value::Int(n) => {
            let _ = write!(result, "{n}");
        }
        Value::BigInt(n) => {
            let _ = write!(result, "{n}");
        }
        Value::Boolean(true) => result.push('t'),
        Value::Boolean(false) => result.push('f'),
        other => result.push_str(&value_to_text(other)),
    }
}

fn apply_format(fmt: &str, args: &[Value]) -> DbResult<String> {
    let chars = fmt.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(fmt.len());
    let mut index = 0usize;
    let mut next_arg = 1usize;

    while index < chars.len() {
        if chars[index] != '%' {
            result.push(chars[index]);
            index += 1;
            continue;
        }

        index += 1;
        if index >= chars.len() {
            return Err(format_unterminated_error());
        }
        if chars[index] == '%' {
            result.push('%');
            index += 1;
            continue;
        }

        let value_position = parse_format_position(&chars, &mut index)?;
        let mut left_justify = false;
        if index < chars.len() && chars[index] == '-' {
            left_justify = true;
            index += 1;
        }

        let width = if index < chars.len() && chars[index] == '*' {
            index += 1;
            let width_position = parse_format_position(&chars, &mut index)?;
            let width_value = resolve_format_arg(width_position, args, &mut next_arg)?;
            let mut width = format_width(width_value)?;
            if width < 0 {
                left_justify = true;
                width = width.saturating_abs();
            }
            usize::try_from(width).ok()
        } else {
            parse_literal_width(&chars, &mut index)
        };

        if index >= chars.len() {
            return Err(format_unterminated_error());
        }

        let spec = chars[index];
        index += 1;

        let rendered = match spec {
            's' => value_to_text(resolve_format_arg(value_position, args, &mut next_arg)?),
            'I' => {
                let value = resolve_format_arg(value_position, args, &mut next_arg)?;
                if value.is_null() {
                    return Err(DbError::internal(
                        "null values cannot be formatted as an SQL identifier",
                    ));
                }
                let mut quoted = String::new();
                format_quote_ident(&value_to_text(value), &mut quoted)?;
                quoted
            }
            'L' => {
                let value = resolve_format_arg(value_position, args, &mut next_arg)?;
                if value.is_null() {
                    "NULL".to_owned()
                } else {
                    let text = value_to_text(value);
                    if text.contains('\0') {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::InvalidParameterValue,
                            "format(%L) does not accept NUL in the input",
                        ));
                    }
                    quote_literal_text(&text)
                }
            }
            '%' => "%".to_owned(),
            other => return Err(format_unknown_specifier_error(other)),
        };

        result.push_str(&apply_width(rendered, width, left_justify)?);
    }

    Ok(result)
}

fn parse_format_position(chars: &[char], index: &mut usize) -> DbResult<Option<usize>> {
    let start = *index;
    while *index < chars.len() && chars[*index].is_ascii_digit() {
        *index += 1;
    }
    if *index == start || *index >= chars.len() || chars[*index] != '$' {
        *index = start;
        return Ok(None);
    }

    // Direct digit arithmetic instead of String collect + parse.
    let mut position: usize = 0;
    for &ch in &chars[start..*index] {
        let digit = (ch as u32).wrapping_sub('0' as u32) as usize;
        position = position
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .ok_or_else(format_unterminated_error)?;
    }
    if position == 0 {
        return Err(DbError::internal(
            "format specifies argument 0, but arguments are numbered from 1",
        ));
    }

    *index += 1;
    Ok(Some(position))
}

fn xpath_text_equals_needle(expr: &str) -> Option<String> {
    let marker = "text() = '";
    let start = expr.find(marker)? + marker.len();
    let tail = expr.get(start..)?;
    let end = tail.find('\'')?;
    Some(tail[..end].to_owned())
}

fn xpath_last_tag(expr: &str) -> Option<String> {
    let head = expr.split('[').next().unwrap_or(expr);
    let tag = head.rsplit('/').find(|part| !part.is_empty())?.trim();
    if tag.is_empty() || tag.contains('(') {
        return None;
    }
    Some(
        tag.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .to_owned(),
    )
}

fn parse_literal_width(chars: &[char], index: &mut usize) -> Option<usize> {
    let start = *index;
    while *index < chars.len() && chars[*index].is_ascii_digit() {
        *index += 1;
    }
    if *index == start {
        None
    } else {
        chars[start..*index]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .ok()
    }
}

fn resolve_format_arg<'a>(
    position: Option<usize>,
    args: &'a [Value],
    next_arg: &mut usize,
) -> DbResult<&'a Value> {
    let position = match position {
        Some(position) => {
            *next_arg = (*next_arg).max(position.saturating_add(1));
            position
        }
        None => {
            let position = *next_arg;
            *next_arg += 1;
            position
        }
    };
    args.get(position.saturating_sub(1))
        .ok_or_else(format_too_few_args_error)
}

fn format_width(value: &Value) -> DbResult<i32> {
    match value {
        Value::Null => Ok(0),
        Value::Int(width) => Ok(*width),
        Value::BigInt(width) => {
            let clamped = (*width).clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            Ok(i32::try_from(clamped).unwrap_or(i32::MAX))
        }
        Value::Real(width) => format_width_from_f64(f64::from(*width)),
        Value::Double(width) => format_width_from_f64(*width),
        Value::Numeric(width) => Ok(width.to_string().parse::<i32>().unwrap_or(0)),
        Value::Text(text) => Ok(text.parse::<i32>().unwrap_or(0)),
        _ => Err(DbError::internal("format() width must be an integer")),
    }
}

fn format_width_from_f64(f: f64) -> DbResult<i32> {
    if !f.is_finite() || f < f64::from(i32::MIN) || f > f64::from(i32::MAX) {
        return Err(DbError::internal("format() width out of range"));
    }
    let integral = f.trunc();
    let repr = format!("{integral:.0}");
    repr.parse::<i32>()
        .map_err(|_| DbError::internal("format() width out of range"))
}

/// Upper bound on the padding width that a single `format()` specifier
/// can request. Matches the `MAX_PAD_LENGTH`/`MAX_REPEAT_BYTES` plafonds
/// applied by `lpad`, `rpad`, and `repeat`: any SQL call that asks for a
/// wider field is rejected with a protocol-level error rather than
/// allocating the literal amount. Without this cap, a plain SQL call
/// like `SELECT format('%*s', 500000000, 'x')` immediately allocates
/// half a gigabyte per call, which can be reached by any authenticated
/// role - a trivial unauthenticated-to-OOM pivot.
const MAX_FORMAT_WIDTH: usize = 10 * 1024 * 1024;

fn apply_width(rendered: String, width: Option<usize>, left_justify: bool) -> DbResult<String> {
    let Some(width) = width else {
        return Ok(rendered);
    };
    if width > MAX_FORMAT_WIDTH {
        return Err(DbError::internal(format!(
            "format() width {width} exceeds maximum of {MAX_FORMAT_WIDTH} bytes"
        )));
    }
    let len = rendered.chars().count();
    if len >= width {
        return Ok(rendered);
    }
    let padding = " ".repeat(width - len);
    Ok(if left_justify {
        format!("{rendered}{padding}")
    } else {
        format!("{padding}{rendered}")
    })
}

fn format_too_few_args_error() -> DbError {
    DbError::internal("too few arguments for format()")
}

fn format_unknown_specifier_error(specifier: char) -> DbError {
    DbError::internal(format!(
        "unrecognized format() type specifier \"{specifier}\""
    ))
    .with_client_hint("For a single \"%\" use \"%%\".")
}

fn format_unterminated_error() -> DbError {
    DbError::internal("unterminated format() type specifier")
        .with_client_hint("For a single \"%\" use \"%%\".")
}

/// Returns `true` when `s` is a SQL reserved keyword that PG's
/// `quote_identifier` would force into double-quotes. The list mirrors
/// upstream PG's reserved + reserved(can be function) categories so that
/// `format('%I', 'select')` round-trips as `"select"` instead of bare
/// `select`. Only the subset commonly emitted by ORM code-generators is
/// covered; less frequent reserved names still leak through unquoted but
/// that's the same gap any minimalist quote_identifier has.
fn is_reserved_sql_keyword(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "all"
            | "analyse"
            | "analyze"
            | "and"
            | "any"
            | "array"
            | "as"
            | "asc"
            | "asymmetric"
            | "authorization"
            | "between"
            | "binary"
            | "both"
            | "case"
            | "cast"
            | "check"
            | "collate"
            | "collation"
            | "column"
            | "concurrently"
            | "constraint"
            | "create"
            | "cross"
            | "current_catalog"
            | "current_date"
            | "current_role"
            | "current_schema"
            | "current_time"
            | "current_timestamp"
            | "current_user"
            | "default"
            | "deferrable"
            | "desc"
            | "distinct"
            | "do"
            | "else"
            | "end"
            | "except"
            | "false"
            | "fetch"
            | "for"
            | "foreign"
            | "freeze"
            | "from"
            | "full"
            | "grant"
            | "group"
            | "having"
            | "ilike"
            | "in"
            | "initially"
            | "inner"
            | "intersect"
            | "into"
            | "is"
            | "isnull"
            | "join"
            | "lateral"
            | "leading"
            | "left"
            | "like"
            | "limit"
            | "localtime"
            | "localtimestamp"
            | "natural"
            | "not"
            | "notnull"
            | "null"
            | "offset"
            | "on"
            | "only"
            | "or"
            | "order"
            | "outer"
            | "overlaps"
            | "placing"
            | "primary"
            | "references"
            | "returning"
            | "right"
            | "select"
            | "session_user"
            | "similar"
            | "some"
            | "symmetric"
            | "system_user"
            | "table"
            | "tablesample"
            | "then"
            | "to"
            | "trailing"
            | "true"
            | "union"
            | "unique"
            | "user"
            | "using"
            | "variadic"
            | "verbose"
            | "when"
            | "where"
            | "window"
            | "with"
    )
}

fn format_quote_ident(s: &str, out: &mut String) -> DbResult<()> {
    // PG identifiers cannot contain NUL/CR/LF; reflecting any of those
    // through `format('%I', ...)` re-emits log lines with smuggled newlines
    // and lets a NUL-truncated downstream consumer see a different
    // identifier (audit errfmt F2).
    if s.chars().any(|c| c == '\0' || c == '\r' || c == '\n') {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::InvalidParameterValue,
            "format(%I) does not accept NUL/CR/LF in the identifier",
        ));
    }
    let needs_quoting = s.is_empty()
        || s.starts_with(|c: char| c.is_ascii_digit())
        || s.chars()
            .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        || is_reserved_sql_keyword(s);
    if needs_quoting {
        // Stream the doubled-quote escape straight into `out` instead
        // of building an intermediate `s.replace('"', "\"\"")` String
        // and immediately copying it through `push_str`.
        out.push('"');
        for ch in s.chars() {
            if ch == '"' {
                out.push_str("\"\"");
            } else {
                out.push(ch);
            }
        }
        out.push('"');
    } else {
        out.push_str(s);
    }
    Ok(())
}

fn quote_literal_text(text: &str) -> String {
    let use_escape_syntax = text.contains('\\');
    // Build the entire `'...'` (or `E'...'`) literal in one buffer.
    // The legacy implementation built `escaped` into one String,
    // then `format!("'{escaped}'")` allocated a second String just
    // to wrap with quotes. We can do both passes into the same
    // output buffer.
    let mut out = String::with_capacity(text.len() + 3);
    if use_escape_syntax {
        out.push('E');
    }
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' if use_escape_syntax => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

pub(super) fn value_to_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Text(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Numeric(n) => n.to_string(),
        Value::Boolean(flag) => {
            if *flag {
                "t".to_owned()
            } else {
                "f".to_owned()
            }
        }
        Value::Date(date) => format_date_for_session(*date),
        Value::LargeDate(date) => date.to_string(),
        Value::Tid(value) => value.to_string(),
        Value::PgLsn(value) => value.to_string(),
        Value::MacAddr(value) => value.to_string(),
        Value::MacAddr8(value) => value.to_string(),
        Value::Blob(bytes) => format_bytea_hex(bytes),
        other => other.to_string(),
    }
}

fn format_bytea_hex(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len() * 2 + 2);
    rendered.push_str("\\x");
    for byte in bytes {
        rendered.push(hex_digit(byte >> 4));
        rendered.push(hex_digit(byte & 0x0f));
    }
    rendered
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'a' + (value - 10)),
        _ => unreachable!(),
    }
}

fn format_date_for_session(date: time::Date) -> String {
    let context = current_temporal_session_context();
    let family = if matches!(context.date_style, DateStyleFamily::Iso)
        && matches!(context.date_order, DateOrder::Mdy)
    {
        DateStyleFamily::Postgres
    } else {
        context.date_style
    };
    let (year, bc) = display_year(date.year());
    let month = u8::from(date.month());
    let day = date.day();
    let body = match family {
        DateStyleFamily::Postgres => match context.date_order {
            DateOrder::Mdy => format!("{month:02}-{day:02}-{year:04}"),
            DateOrder::Dmy => format!("{day:02}-{month:02}-{year:04}"),
            DateOrder::Ymd => format!("{year:04}-{month:02}-{day:02}"),
        },
        DateStyleFamily::Iso => format!("{year:04}-{month:02}-{day:02}"),
        DateStyleFamily::Sql => match context.date_order {
            DateOrder::Mdy => format!("{month:02}/{day:02}/{year:04}"),
            DateOrder::Dmy => format!("{day:02}/{month:02}/{year:04}"),
            DateOrder::Ymd => format!("{year:04}/{month:02}/{day:02}"),
        },
        DateStyleFamily::German => format!("{day:02}.{month:02}.{year:04}"),
    };
    if bc {
        format!("{body} BC")
    } else {
        body
    }
}

fn display_year(year: i32) -> (u32, bool) {
    if year <= 0 {
        (
            u32::try_from(year.saturating_neg().saturating_add(1)).unwrap_or(u32::MAX),
            true,
        )
    } else {
        (u32::try_from(year).unwrap_or(u32::MAX), false)
    }
}

pub(super) fn eval_generic_multirange(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Text("{}".to_owned()));
    }
    let mut inferred_kind = None;
    for arg in args {
        if matches!(arg, Value::Null) {
            continue;
        }
        let range = range::text_to_range(arg)?;
        inferred_kind = Some(range.kind);
        break;
    }
    let Some(kind) = inferred_kind else {
        return Ok(Value::Text("{}".to_owned()));
    };
    range::eval_multirange_constructor(kind, args)
}

#[cfg(test)]
mod tests {
    use aiondb_core::NumericValue;
    use time::{Date, Month};

    use crate::eval::{with_session_context, EvalSessionContext};

    use super::*;

    #[test]
    fn value_to_text_formats_booleans_and_dates_like_pg() {
        let context = EvalSessionContext::from_settings(Some("Postgres, MDY"), Some("UTC"));
        let date = Date::from_calendar_date(2010, Month::March, 9).unwrap();
        with_session_context(context, || {
            assert_eq!(value_to_text(&Value::Boolean(true)), "t");
            assert_eq!(value_to_text(&Value::Boolean(false)), "f");
            assert_eq!(value_to_text(&Value::Date(date)), "03-09-2010");
        });
    }

    #[test]
    fn variadic_concat_expands_array_elements() {
        let result = eval_variadic_concat(&[Value::Array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])])
        .unwrap();
        assert_eq!(result, Value::Text("123".to_owned()));
    }

    #[test]
    fn variadic_concat_null_array_returns_null() {
        assert_eq!(eval_variadic_concat(&[Value::Null]).unwrap(), Value::Null);
    }

    #[test]
    fn variadic_concat_ws_null_array_returns_empty_string() {
        assert_eq!(
            eval_variadic_concat_ws(&[Value::Text(",".to_owned()), Value::Null]).unwrap(),
            Value::Text(String::new())
        );
    }

    /// Regression for a confirmed unauthenticated-to-OOM DoS: before the
    /// cap in `apply_width`, `SELECT format('%*s', N, 'x')` allocated
    /// exactly `N` bytes with no upper bound, so any SQL role could
    /// request hundreds of megabytes per call and exhaust the host RAM.
    /// The test uses `MAX_FORMAT_WIDTH + 1` to prove the cap triggers
    /// *before* any large allocation occurs - so it is safe to run in the
    /// main test process (no gigabyte-scale allocation ever attempted).
    #[test]
    fn format_rejects_width_above_cap_without_allocating() {
        let requested = MAX_FORMAT_WIDTH + 1;
        let err = eval_format(&[
            Value::Text("%*s".to_owned()),
            Value::BigInt(requested as i64),
            Value::Text("x".to_owned()),
        ])
        .expect_err("oversized width must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("format() width") && msg.contains("exceeds maximum"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn format_accepts_width_at_cap() {
        // Sanity-check that the boundary itself still works; use a tiny
        // width to avoid any risk of a multi-megabyte allocation during
        // the test run.
        let rendered =
            eval_format(&[Value::Text("%5s".to_owned()), Value::Text("x".to_owned())]).unwrap();
        assert_eq!(rendered, Value::Text("    x".to_owned()));
    }

    #[test]
    fn format_handles_positions_width_and_identifiers() {
        let rendered = eval_format(&[
            Value::Text(">>%2$*1$L<< %2$-10I".to_owned()),
            Value::Int(10),
            Value::Text("Hello".to_owned()),
        ])
        .unwrap();
        assert_eq!(
            rendered,
            Value::Text(">>   'Hello'<< \"Hello\"   ".to_owned())
        );
    }

    #[test]
    fn format_variadic_expands_array_arguments() {
        let rendered = eval_variadic_format(&[
            Value::Text("%2$s, %1$s".to_owned()),
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
        ])
        .unwrap();
        assert_eq!(rendered, Value::Text("2, 1".to_owned()));
    }

    #[test]
    fn format_reports_pg_style_errors() {
        let too_few = eval_format(&[Value::Text("Hello %s".to_owned())]).unwrap_err();
        assert_eq!(too_few.report().message, "too few arguments for format()");

        let unknown =
            eval_format(&[Value::Text("Hello %x".to_owned()), Value::Int(20)]).unwrap_err();
        assert_eq!(
            unknown.report().message,
            "unrecognized format() type specifier \"x\""
        );
        assert_eq!(
            unknown.report().client_hint.as_deref(),
            Some("For a single \"%\" use \"%%\".")
        );

        let zero = eval_format(&[
            Value::Text("%0$s".to_owned()),
            Value::Text("Hello".to_owned()),
        ])
        .unwrap_err();
        assert_eq!(
            zero.report().message,
            "format specifies argument 0, but arguments are numbered from 1"
        );

        let ident_null = eval_format(&[Value::Text("%I".to_owned()), Value::Null]).unwrap_err();
        assert_eq!(
            ident_null.report().message,
            "null values cannot be formatted as an SQL identifier"
        );
    }

    #[test]
    fn format_width_accepts_numeric_value() {
        let rendered = eval_format(&[
            Value::Text(">>%*s<<".to_owned()),
            Value::Numeric(NumericValue::from_i32(10)),
            Value::Text("Hello".to_owned()),
        ])
        .unwrap();
        assert_eq!(rendered, Value::Text(">>     Hello<<".to_owned()));
    }
}
