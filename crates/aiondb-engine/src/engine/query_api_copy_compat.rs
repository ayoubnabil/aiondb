use std::collections::HashSet;

use aiondb_core::{DataType, Value};

pub(in crate::engine) fn escape_copy_text_value(value: &str) -> String {
    if !value
        .as_bytes()
        .iter()
        .any(|b| matches!(*b, b'\\' | b'\t' | b'\n' | b'\r'))
    {
        return value.to_owned();
    }
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

pub(in crate::engine) fn unescape_copy_text_value(value: &str) -> String {
    if !value.as_bytes().contains(&b'\\') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

pub(in crate::engine) fn format_copy_text_value(value: &Value) -> String {
    aiondb_executor::format_copy_text_value(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::engine) enum CopyCompatFormat {
    Text,
    Csv,
    Binary,
}

#[derive(Clone, Debug)]
pub(in crate::engine) struct CopyCompatOptions {
    pub(in crate::engine) format: CopyCompatFormat,
    pub(in crate::engine) delimiter: char,
    pub(in crate::engine) delimiter_explicit: bool,
    pub(in crate::engine) null_string: String,
    pub(in crate::engine) null_explicit: bool,
    pub(in crate::engine) default_string: Option<String>,
    pub(in crate::engine) header: bool,
    pub(in crate::engine) header_match: bool,
    pub(in crate::engine) quote: char,
    pub(in crate::engine) escape: char,
    pub(in crate::engine) force_quote_all: bool,
    pub(in crate::engine) force_quote_columns: HashSet<String>,
    pub(in crate::engine) force_not_null_columns: HashSet<String>,
    pub(in crate::engine) force_null_columns: HashSet<String>,
    pub(in crate::engine) where_clause: Option<String>,
}

impl CopyCompatOptions {
    pub(in crate::engine) fn for_direction(direction: aiondb_parser::CopyDirection) -> Self {
        let (delimiter, null_string) = match direction {
            aiondb_parser::CopyDirection::From | aiondb_parser::CopyDirection::To => {
                ('\t', "\\N".to_owned())
            }
        };
        Self {
            format: CopyCompatFormat::Text,
            delimiter,
            delimiter_explicit: false,
            null_string,
            null_explicit: false,
            default_string: None,
            header: false,
            header_match: false,
            quote: '"',
            escape: '"',
            force_quote_all: false,
            force_quote_columns: HashSet::new(),
            force_not_null_columns: HashSet::new(),
            force_null_columns: HashSet::new(),
            where_clause: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::engine) enum CopyWhereOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone, Debug)]
pub(in crate::engine) struct CopyWherePredicate {
    pub(in crate::engine) column: String,
    pub(in crate::engine) op: CopyWhereOp,
    pub(in crate::engine) literal: String,
}

#[derive(Clone, Debug)]
pub(in crate::engine) struct CopyColumnCompat {
    pub(in crate::engine) name: String,
    pub(in crate::engine) data_type: DataType,
    pub(in crate::engine) text_type_modifier: Option<aiondb_core::TextTypeModifier>,
    pub(in crate::engine) nullable: bool,
    pub(in crate::engine) has_default: bool,
    pub(in crate::engine) default_value: Option<String>,
}

#[derive(Clone, Debug)]
pub(in crate::engine) struct CopyCsvField {
    pub(in crate::engine) value: String,
    pub(in crate::engine) quoted: bool,
}

pub(in crate::engine) fn format_copy_csv_value(
    value: &Value,
    force_quote: bool,
    options: &CopyCompatOptions,
) -> String {
    if matches!(value, Value::Null) {
        return options.null_string.clone();
    }
    let raw = value.to_string();
    let needs_quote = force_quote
        || raw.is_empty()
        || raw == "\\."
        || csv_needs_quote(raw.as_bytes(), options.delimiter, options.quote);
    if !needs_quote {
        return raw;
    }

    let mut escaped = String::with_capacity(raw.len() + 2);
    escaped.push(options.quote);
    for ch in raw.chars() {
        if options.escape == options.quote {
            if ch == options.quote {
                escaped.push(options.quote);
            }
            escaped.push(ch);
            continue;
        }

        if ch == options.quote || ch == options.escape {
            escaped.push(options.escape);
        }
        escaped.push(ch);
    }
    escaped.push(options.quote);
    escaped
}

fn csv_needs_quote(bytes: &[u8], delimiter: char, quote: char) -> bool {
    if delimiter.is_ascii() && quote.is_ascii() {
        let delim = delimiter as u8;
        let quote = quote as u8;
        return bytes
            .iter()
            .any(|b| *b == delim || *b == quote || *b == b'\n' || *b == b'\r');
    }
    let s = std::str::from_utf8(bytes).unwrap_or("");
    s.contains(delimiter) || s.contains(quote) || s.contains('\n') || s.contains('\r')
}

pub(in crate::engine) fn split_top_level_csv_items(sql: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut cursor = 0usize;
    let mut start = 0usize;
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;
    let mut in_single_quote = false;
    let bytes = sql.as_bytes();

    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if in_single_quote {
            if ch == b'\'' {
                if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                    cursor += 2;
                    continue;
                }
                in_single_quote = false;
            }
            cursor += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                cursor += 1;
            }
            b'(' => {
                paren_depth += 1;
                cursor += 1;
            }
            b')' => {
                paren_depth = paren_depth.checked_sub(1)?;
                cursor += 1;
            }
            b'[' => {
                bracket_depth += 1;
                cursor += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.checked_sub(1)?;
                cursor += 1;
            }
            b',' if paren_depth == 0 && bracket_depth == 0 => {
                items.push(sql[start..cursor].trim().to_owned());
                cursor += 1;
                start = cursor;
            }
            _ => cursor += 1,
        }
    }

    let tail = sql[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    Some(items)
}

pub(in crate::engine) fn decode_sql_single_quoted_literal(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("E'") {
        rest.strip_suffix('\'')?
    } else if let Some(rest) = trimmed.strip_prefix("e'") {
        rest.strip_suffix('\'')?
    } else {
        trimmed.strip_prefix('\'')?.strip_suffix('\'')?
    };

    let mut out = String::new();
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' && chars.peek().copied() == Some('\'') {
            out.push('\'');
            chars.next();
            continue;
        }
        if matches!(trimmed.as_bytes().first(), Some(b'E' | b'e')) && ch == '\\' {
            let Some(next) = chars.next() else {
                out.push('\\');
                break;
            };
            match next {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            continue;
        }
        out.push(ch);
    }
    Some(out)
}

pub(in crate::engine) fn object_name_to_qualified(
    name: &aiondb_parser::ObjectName,
) -> aiondb_catalog::QualifiedName {
    match name.parts.as_slice() {
        [relation] => aiondb_catalog::QualifiedName::unqualified(relation.clone()),
        [schema, relation] => {
            aiondb_catalog::QualifiedName::qualified(schema.clone(), relation.clone())
        }
        parts => {
            let relation = parts.last().cloned().unwrap_or_default();
            aiondb_catalog::QualifiedName::unqualified(relation)
        }
    }
}
