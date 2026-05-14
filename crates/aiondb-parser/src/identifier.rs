use crate::keywords::lookup_keyword;

pub fn identifier_requires_quotes(identifier: &str) -> bool {
    let mut chars = identifier.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_lowercase() => {}
        _ => return true,
    }

    if chars.any(|ch| !(ch == '_' || ch.is_ascii_lowercase() || ch.is_ascii_digit())) {
        return true;
    }

    lookup_keyword(identifier).is_some_and(|keyword| !keyword.is_unreserved())
}

pub fn quote_identifier(identifier: &str) -> String {
    if identifier.is_empty() || identifier_requires_quotes(identifier) {
        return format!("\"{}\"", identifier.replace('"', "\"\""));
    }
    identifier.to_owned()
}

/// Returns `true` when `name` matches one of the PostgreSQL system columns
/// (`ctid`, `oid`, `xmin`, `xmax`, `cmin`, `cmax`, `tableoid`). Comparison is
/// ASCII case-insensitive — `tableoid` and `TABLEOID` both match.
pub fn is_system_column_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "ctid" | "oid" | "xmin" | "xmax" | "cmin" | "cmax" | "tableoid"
    )
}
