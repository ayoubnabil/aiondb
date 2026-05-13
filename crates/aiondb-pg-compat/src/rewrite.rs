//! Pure SQL text rewrites applied before the parser sees user input, plus
//! small post-parse string fix-ups that keep AionDB compatible with
//! PostgreSQL wire idioms (large-object mode literals, owner-qualified
//! sequence defaults, multirange type helpers, etc.).

use std::path::PathBuf;

use crate::scan::{
    consume_word_ci, find_ascii_case_insensitive, parse_compat_identifier,
    replace_ascii_case_insensitive_all, skip_sql_whitespace,
};

pub fn sql_may_require_preparse_rewrite(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    trimmed
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("copy"))
        || find_ascii_case_insensitive(sql, "current of").is_some()
        || find_ascii_case_insensitive(sql, "execute").is_some()
        || (find_ascii_case_insensitive(sql, "create").is_some()
            && find_ascii_case_insensitive(sql, "schema").is_some()
            && find_ascii_case_insensitive(sql, "authorization").is_some()
            && (find_ascii_case_insensitive(sql, "current_role").is_some()
                || find_ascii_case_insensitive(sql, "current_user").is_some()
                || find_ascii_case_insensitive(sql, "session_user").is_some()))
        || (find_ascii_case_insensitive(sql, "lo_open").is_some()
            && find_ascii_case_insensitive(sql, "x'").is_some())
}

pub fn rewrite_largeobject_mode_literals(sql: &str) -> Option<String> {
    let mut rewritten = sql.to_owned();
    let mut changed = false;

    for (needle, replacement) in [
        ("cast(x'20000' | x'40000' as integer)", "393216"),
        ("x'40000'::int", "262144"),
        ("x'20000'::int", "131072"),
    ] {
        let (next, did_replace) =
            replace_ascii_case_insensitive_all(&rewritten, needle, replacement);
        rewritten = next;
        changed |= did_replace;
    }

    changed.then_some(rewritten)
}

pub fn rewrite_owned_sequence_schema_in_default_expr(
    default_expr: &str,
    old_schema: &str,
    new_schema: &str,
    sequence_name: &str,
) -> String {
    let mut rewritten = default_expr.to_owned();
    let old_plain = format!("'{old_schema}.{sequence_name}'");
    let new_plain = format!("'{new_schema}.{sequence_name}'");
    let old_quoted = format!("'\"{old_schema}\".\"{sequence_name}\"'");
    for (needle, replacement) in [(&old_plain, &new_plain), (&old_quoted, &new_plain)] {
        let (next, _changed) = replace_ascii_case_insensitive_all(&rewritten, needle, replacement);
        rewritten = next;
    }
    rewritten
}

pub fn compat_statement_sql_fragment(sql: &str, span: aiondb_parser::Span) -> Option<&str> {
    let mut end = span.end.min(sql.len());
    if let Some(suffix) = sql.get(end..) {
        if let Some(semicolon_offset) = suffix.find(';') {
            end += semicolon_offset;
        } else {
            end = sql.len();
        }
    }
    sql.get(span.start..end).map(str::trim)
}

pub fn compat_cursor_statement_name(portal_name: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    portal_name.hash(&mut hasher);
    format!("__compat_cursor_{:016x}", hasher.finish())
}

pub fn object_name_matches_relation_name(
    object_name: &aiondb_parser::ObjectName,
    relation_name: &str,
) -> bool {
    object_name.parts.len() == 1 && object_name.parts[0].eq_ignore_ascii_case(relation_name)
}

pub fn parse_copy_string_literal(source: &str) -> Option<PathBuf> {
    let source = source.trim();
    if !(source.starts_with('\'') && source.ends_with('\'')) {
        return None;
    }
    let inner = source[1..source.len() - 1].replace("''", "'");
    Some(PathBuf::from(inner))
}

pub fn parse_copy_psql_variable(source: &str) -> Option<String> {
    let source = source.trim();
    if !(source.starts_with(":'") && source.ends_with('\'')) {
        return None;
    }
    Some(source[2..source.len() - 1].to_owned())
}

pub fn parse_multirange_type_name_option(options: &str) -> Option<String> {
    for part in options.split(',') {
        let mut cursor = 0usize;
        if consume_word_ci(part, &mut cursor, "multirange_type_name").is_none() {
            continue;
        }
        skip_sql_whitespace(part, &mut cursor);
        if !part.get(cursor..)?.starts_with('=') {
            continue;
        }
        cursor += 1;
        if let Some(name) = parse_compat_identifier(part, &mut cursor) {
            return Some(name);
        }
    }
    None
}

pub fn default_multirange_type_name(range_type_name: &str) -> String {
    let normalized = aiondb_eval::normalize_compat_type_name(range_type_name);
    if let Some(prefix) = normalized.strip_suffix("range") {
        format!("{prefix}multirange")
    } else {
        format!("{normalized}_multirange")
    }
}
