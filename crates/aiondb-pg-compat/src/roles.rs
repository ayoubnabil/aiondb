//! Parsers for PostgreSQL-compatible role statements that only need to
//! extract role names (`DROP OWNED BY`, `REASSIGN OWNED BY`, `DROP ROLE`).
//! The engine applies privilege/ownership effects; this module is pure.

use crate::scan::{
    consume_word_ci, parse_identifier_part, skip_sql_whitespace, trim_compat_statement,
};

pub fn parse_drop_owned_roles(statement_sql: &str) -> Option<Vec<String>> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "owned")?;
    consume_word_ci(sql, &mut cursor, "by")?;

    let mut roles = Vec::new();
    loop {
        let role_name = parse_identifier_part(sql, &mut cursor)?;
        roles.push(role_name);
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with(',') {
            cursor += 1;
            continue;
        }
        break;
    }

    let _ = consume_word_ci(sql, &mut cursor, "cascade")
        .or_else(|| consume_word_ci(sql, &mut cursor, "restrict"));
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }

    Some(roles)
}

/// Parses a `REASSIGN OWNED BY <old>, ... TO <new>` statement and returns
/// the list of source role names alongside the target role name.
pub fn parse_reassign_owned_roles(statement_sql: &str) -> Option<(Vec<String>, String)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "reassign")?;
    consume_word_ci(sql, &mut cursor, "owned")?;
    consume_word_ci(sql, &mut cursor, "by")?;

    let mut sources = Vec::new();
    loop {
        let role_name = parse_identifier_part(sql, &mut cursor)?;
        sources.push(role_name);
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with(',') {
            cursor += 1;
            continue;
        }
        break;
    }
    consume_word_ci(sql, &mut cursor, "to")?;
    let target = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }

    Some((sources, target))
}

pub fn parse_drop_role_names(statement_sql: &str) -> Option<(Vec<String>, bool)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    if consume_word_ci(sql, &mut cursor, "role").is_none()
        && consume_word_ci(sql, &mut cursor, "user").is_none()
        && consume_word_ci(sql, &mut cursor, "group").is_none()
    {
        return None;
    }
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };

    let mut role_names = Vec::new();
    loop {
        role_names.push(parse_identifier_part(sql, &mut cursor)?);
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with(',') {
            cursor += 1;
            continue;
        }
        break;
    }
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some((role_names, if_exists))
}
