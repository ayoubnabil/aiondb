//! Parsers for PostgreSQL-compatible type references used in signatures
//! of CREATE/DROP CAST, CREATE/DROP FUNCTION, CREATE/DROP PROCEDURE,
//! CREATE/DROP AGGREGATE and CREATE/DROP OPERATOR statements.
//!
//! Pure: no engine coupling. The engine maps parsed signatures to catalog
//! descriptors and resolves overloads itself.

use crate::scan::{
    consume_word_ci, find_ascii_case_insensitive, parse_identifier_part, skip_sql_whitespace,
    trim_compat_statement,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatTypeRef {
    pub schema_name: Option<String>,
    pub type_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatDropRoutineKind {
    Procedure,
    Routine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatDropRoutine {
    pub kind: CompatDropRoutineKind,
    pub routine_name: String,
    pub if_exists: bool,
    pub has_signature: bool,
}

pub fn parse_type_reference(sql: &str, cursor: &mut usize) -> Option<String> {
    let mut type_name = parse_identifier_part(sql, cursor)?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..)?.starts_with('.') {
        *cursor += 1;
        type_name = parse_identifier_part(sql, cursor)?;
    }
    Some(type_name)
}

pub fn parse_qualified_type_reference(
    sql: &str,
    cursor: &mut usize,
) -> Option<ParsedCompatTypeRef> {
    let mut schema_name = None;
    let mut type_name = parse_identifier_part(sql, cursor)?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..)?.starts_with('.') {
        schema_name = Some(type_name.to_ascii_lowercase());
        *cursor += 1;
        type_name = parse_identifier_part(sql, cursor)?;
        skip_sql_whitespace(sql, cursor);
    }
    while sql.get(*cursor..).is_some_and(|rest| rest.starts_with('[')) {
        *cursor += 1;
        skip_sql_whitespace(sql, cursor);
        while *cursor < sql.len() {
            let ch = sql[*cursor..].chars().next()?;
            if ch == ']' {
                *cursor += 1;
                break;
            }
            *cursor += ch.len_utf8();
        }
        type_name.push_str("[]");
        skip_sql_whitespace(sql, cursor);
    }
    Some(ParsedCompatTypeRef {
        schema_name,
        type_name: type_name.to_ascii_lowercase(),
    })
}

pub fn parse_type_ref_list_until_rparen(
    sql: &str,
    cursor: &mut usize,
    allow_none: bool,
) -> Option<Vec<ParsedCompatTypeRef>> {
    let mut refs = Vec::new();
    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..)?.starts_with('(') {
        return None;
    }
    *cursor += 1;
    loop {
        skip_sql_whitespace(sql, cursor);
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
            *cursor += 1;
            break;
        }
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with('*')) {
            *cursor += 1;
            skip_sql_whitespace(sql, cursor);
            if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
                *cursor += 1;
                break;
            }
            return None;
        }
        let start = *cursor;
        let parsed = parse_qualified_type_reference(sql, cursor).or_else(|| {
            if allow_none {
                let token = parse_identifier_part(sql, cursor)?;
                if token.eq_ignore_ascii_case("none") {
                    return Some(ParsedCompatTypeRef {
                        schema_name: None,
                        type_name: "none".to_owned(),
                    });
                }
            }
            None
        });
        if let Some(type_ref) = parsed {
            if type_ref.type_name != "none" {
                refs.push(type_ref);
            }
        } else if *cursor == start {
            return None;
        }
        skip_sql_whitespace(sql, cursor);
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(',')) {
            *cursor += 1;
            continue;
        }
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
            *cursor += 1;
            break;
        }
        return None;
    }
    Some(refs)
}

pub fn parse_drop_function_cascade_name(statement_sql: &str) -> Option<String> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "function")?;
    let _ = consume_word_ci(sql, &mut cursor, "if").and_then(|()| {
        consume_word_ci(sql, &mut cursor, "exists")?;
        Some(())
    });
    let function_name = parse_type_reference(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('(') {
        let mut depth = 0usize;
        while cursor < sql.len() {
            let ch = sql[cursor..].chars().next()?;
            cursor += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
        }
    }
    find_ascii_case_insensitive(sql, " cascade")?;
    Some(function_name.to_ascii_lowercase())
}

pub fn parse_create_procedure_name(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "procedure")?;
    parse_type_reference(sql, &mut cursor).map(|name| name.to_ascii_lowercase())
}

pub fn parse_drop_procedure_or_routine_statement(
    statement_sql: &str,
) -> Option<ParsedCompatDropRoutine> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    let kind = if consume_word_ci(sql, &mut cursor, "procedure").is_some() {
        CompatDropRoutineKind::Procedure
    } else if consume_word_ci(sql, &mut cursor, "routine").is_some() {
        CompatDropRoutineKind::Routine
    } else {
        return None;
    };
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };
    let routine_name = parse_type_reference(sql, &mut cursor)?.to_ascii_lowercase();
    skip_sql_whitespace(sql, &mut cursor);
    let has_signature = sql.get(cursor..).is_some_and(|rest| rest.starts_with('('));
    Some(ParsedCompatDropRoutine {
        kind,
        routine_name,
        if_exists,
        has_signature,
    })
}

pub fn parse_drop_function_if_exists_signature(
    statement_sql: &str,
) -> Option<(String, Vec<ParsedCompatTypeRef>)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "function")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    let function_name = parse_type_reference(sql, &mut cursor)?.to_ascii_lowercase();
    let arg_types = parse_type_ref_list_until_rparen(sql, &mut cursor, false).unwrap_or_default();
    Some((function_name, arg_types))
}

pub fn parse_drop_aggregate_if_exists_signature(
    statement_sql: &str,
) -> Option<(String, Vec<ParsedCompatTypeRef>)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "aggregate")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    let mut aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    }
    let arg_types = parse_type_ref_list_until_rparen(sql, &mut cursor, false).unwrap_or_default();
    Some((aggregate_name.to_ascii_lowercase(), arg_types))
}

pub fn parse_drop_cast_if_exists_types(
    statement_sql: &str,
) -> Option<(ParsedCompatTypeRef, ParsedCompatTypeRef)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source = parse_qualified_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target = parse_qualified_type_reference(sql, &mut cursor)?;
    Some((source, target))
}

pub fn parse_drop_operator_if_exists_arg_types(
    statement_sql: &str,
) -> Option<Vec<ParsedCompatTypeRef>> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "operator")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    let mut depth = 0usize;
    while cursor < sql.len() {
        let ch = sql[cursor..].chars().next()?;
        if ch == '(' {
            depth = 1;
            break;
        }
        cursor += ch.len_utf8();
    }
    if depth == 0 {
        return None;
    }
    parse_type_ref_list_until_rparen(sql, &mut cursor, true)
}

pub fn parse_drop_if_exists_sql_tail(statement_sql: &str) -> Option<&str> {
    let sql = trim_compat_statement(statement_sql);
    let idx = find_ascii_case_insensitive(sql, "if exists")?;
    sql.get(idx + "if exists".len()..).map(str::trim_start)
}
