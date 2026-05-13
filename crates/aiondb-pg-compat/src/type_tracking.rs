//! Parsers that extract information needed to track PostgreSQL-compatible
//! user-defined types (CREATE TYPE / CREATE DOMAIN / ALTER DOMAIN / DROP
//! DOMAIN). These helpers are pure: they return structured data to the
//! engine, which is responsible for applying the effects to its session
//! state.

use aiondb_eval::{
    normalize_compat_type_name, CompatUserType, CompatUserTypeField, DomainConstraint, DomainDef,
};

pub fn compat_type_name_in_use(
    types: &[CompatUserType],
    name: &str,
    skip_oid: Option<i32>,
) -> bool {
    types
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case(name) && Some(entry.oid) != skip_oid)
}

pub fn allocate_compat_type_name(
    types: &[CompatUserType],
    preferred: &str,
    skip_oid: Option<i32>,
) -> String {
    if !compat_type_name_in_use(types, preferred, skip_oid) {
        return preferred.to_owned();
    }
    let mut suffix = 1usize;
    loop {
        let candidate = format!("{preferred}_{suffix}");
        if !compat_type_name_in_use(types, &candidate, skip_oid) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

use crate::scan::{
    consume_word_ci, extract_parenthesized, parse_identifier_part, skip_sql_whitespace,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCreateTypeKind {
    Shell,
    Enum(Vec<String>),
    Composite(Vec<CompatUserTypeField>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCreateTypeTracking {
    pub type_name: String,
    pub kind: ParsedCreateTypeKind,
}

pub enum AlterDomainAction {
    AddConstraint {
        domain_name: String,
        constraint: DomainConstraint,
    },
    DropConstraint {
        domain_name: String,
        constraint_name: String,
    },
    SetNotNull {
        domain_name: String,
    },
    DropNotNull {
        domain_name: String,
    },
    SetDefault {
        domain_name: String,
        default_expr: String,
    },
    DropDefault {
        domain_name: String,
    },
}

pub fn statement_tracks_compat_types(statement: &aiondb_parser::Statement) -> bool {
    statement.compat_tag().is_some_and(|tag| {
        matches!(
            tag,
            "CREATE TYPE" | "CREATE DOMAIN" | "DROP DOMAIN" | "ALTER DOMAIN"
        )
    })
}

pub fn parse_create_type_tracking(statement_sql: &str) -> Option<ParsedCreateTypeTracking> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut type_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql[cursor..].starts_with('.') {
        cursor += 1;
        type_name = parse_identifier_part(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.trim_start().is_empty()
        || sql.get(cursor..)?.trim_start().starts_with(';')
    {
        return Some(ParsedCreateTypeTracking {
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Shell,
        });
    }

    if sql.get(cursor..)?.starts_with('(') {
        return Some(ParsedCreateTypeTracking {
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Composite(Vec::new()),
        });
    }

    consume_word_ci(sql, &mut cursor, "as")?;
    skip_sql_whitespace(sql, &mut cursor);
    if consume_word_ci(sql, &mut cursor, "enum").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        let labels = parse_create_enum_labels_after_enum_keyword(sql, &mut cursor)?;
        return Some(ParsedCreateTypeTracking {
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Enum(labels),
        });
    }
    if sql.get(cursor..)?.starts_with('(') {
        let inner = extract_parenthesized(sql, &mut cursor)?;
        let fields = parse_composite_type_fields(&inner)?;
        return Some(ParsedCreateTypeTracking {
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Composite(fields),
        });
    }
    Some(ParsedCreateTypeTracking {
        type_name: type_name.to_ascii_lowercase(),
        kind: ParsedCreateTypeKind::Shell,
    })
}

fn parse_create_enum_labels_after_enum_keyword(
    sql: &str,
    cursor: &mut usize,
) -> Option<Vec<String>> {
    if !sql.get(*cursor..)?.starts_with('(') {
        return None;
    }
    *cursor += 1;
    let mut labels = Vec::new();
    loop {
        skip_sql_whitespace(sql, cursor);
        let remaining = sql.get(*cursor..)?;
        if remaining.starts_with(')') {
            *cursor += 1;
            break;
        }
        if remaining.starts_with(',') {
            *cursor += 1;
            continue;
        }
        if remaining.starts_with('\'') {
            *cursor += 1;
            let label_start = *cursor;
            while *cursor < sql.len() {
                let ch = sql.get(*cursor..*cursor + 1)?;
                if ch == "'" {
                    if sql.get(*cursor + 1..*cursor + 2) == Some("'") {
                        *cursor += 2;
                    } else {
                        break;
                    }
                } else {
                    *cursor += 1;
                }
            }
            let label = sql[label_start..*cursor].replace("''", "'");
            labels.push(label);
            *cursor += 1;
        } else {
            return None;
        }
    }
    Some(labels)
}

fn parse_composite_type_fields(inner_sql: &str) -> Option<Vec<CompatUserTypeField>> {
    let wrapped_sql = format!("CREATE TABLE __aiondb_type_tmp__ ({inner_sql})");
    let statement = aiondb_parser::parse_prepared_statement(&wrapped_sql).ok()?;
    let aiondb_parser::Statement::CreateTable(table) = statement else {
        return None;
    };
    Some(
        table
            .columns
            .into_iter()
            .map(|column| {
                let raw_type_name = column
                    .raw_type_name
                    .as_deref()
                    .map(normalize_compat_type_name)
                    .or_else(|| Some(normalize_compat_type_name(&column.data_type.to_string())));
                CompatUserTypeField {
                    name: column.name,
                    raw_type_name,
                    data_type: column.data_type,
                }
            })
            .collect(),
    )
}

/// Parse `CREATE DOMAIN name [AS] base_type [NOT NULL] [DEFAULT ...] [CHECK (...)]`
pub fn parse_create_domain_tracking(statement_sql: &str) -> Option<DomainDef> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "domain")?;
    let mut domain_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        domain_name = parse_identifier_part(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let _ = consume_word_ci(sql, &mut cursor, "as");
    skip_sql_whitespace(sql, &mut cursor);
    let _base_type_start = cursor;
    let base_type_ident = parse_identifier_part(sql, &mut cursor)?;
    let mut base_type_str = base_type_ident.clone();
    let saved_cursor = cursor;
    skip_sql_whitespace(sql, &mut cursor);
    if let Some(next_word) = try_peek_word(sql, cursor) {
        if next_word.eq_ignore_ascii_case("varying") || next_word.eq_ignore_ascii_case("precision")
        {
            base_type_str = format!("{base_type_str} {next_word}");
            cursor += next_word.len();
        } else {
            cursor = saved_cursor;
        }
    } else {
        cursor = saved_cursor;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let mut char_length: Option<u32> = None;
    if sql.get(cursor..)?.starts_with('(') {
        cursor += 1;
        skip_sql_whitespace(sql, &mut cursor);
        let num_start = cursor;
        while cursor < sql.len() && sql[cursor..].starts_with(|c: char| c.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor > num_start {
            let num_str = &sql[num_start..cursor];
            if let Ok(n) = num_str.parse::<u32>() {
                let base_upper = base_type_str.to_ascii_uppercase();
                if base_upper.contains("VARCHAR")
                    || base_upper.contains("VARYING")
                    || base_upper == "CHAR"
                    || base_upper == "CHARACTER"
                {
                    char_length = Some(n);
                }
            }
        }
        while cursor < sql.len() && !sql[cursor..].starts_with(')') {
            cursor += 1;
        }
        if cursor < sql.len() {
            cursor += 1;
        }
    }
    skip_sql_whitespace(sql, &mut cursor);
    let mut is_array = false;
    while sql.get(cursor..)?.starts_with('[') {
        is_array = true;
        while cursor < sql.len() && !sql[cursor..].starts_with(']') {
            cursor += 1;
        }
        if cursor < sql.len() {
            cursor += 1;
        }
        skip_sql_whitespace(sql, &mut cursor);
    }

    let mut base_type = map_base_type_name(&base_type_str);
    if is_array {
        base_type.push_str("[]");
    }

    let mut not_null = false;
    let mut default_expr = None;
    let mut constraints = Vec::new();
    let domain_name_lower = domain_name.to_ascii_lowercase();

    while cursor < sql.len() {
        skip_sql_whitespace(sql, &mut cursor);
        let remaining = sql.get(cursor..)?.trim_start();
        if remaining.is_empty() || remaining.starts_with(';') {
            break;
        }
        if consume_word_ci(sql, &mut cursor, "not").is_some()
            && consume_word_ci(sql, &mut cursor, "null").is_some()
        {
            not_null = true;
            continue;
        }
        if consume_word_ci(sql, &mut cursor, "null").is_some() {
            continue;
        }
        if consume_word_ci(sql, &mut cursor, "default").is_some() {
            skip_sql_whitespace(sql, &mut cursor);
            let def_start = cursor;
            while cursor < sql.len() {
                let rest = &sql[cursor..];
                if starts_with_keyword_ci(rest, "check")
                    || starts_with_keyword_ci(rest, "constraint")
                    || starts_with_keyword_ci(rest, "not")
                {
                    break;
                }
                cursor += sql[cursor..].chars().next().map_or(1, |c| c.len_utf8());
            }
            default_expr = Some(sql[def_start..cursor].trim().to_owned());
            continue;
        }
        if consume_word_ci(sql, &mut cursor, "constraint").is_some() {
            let con_name = parse_identifier_part(sql, &mut cursor)?;
            skip_sql_whitespace(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "not").is_some()
                && consume_word_ci(sql, &mut cursor, "null").is_some()
            {
                not_null = true;
                constraints.push(DomainConstraint {
                    name: con_name.to_ascii_lowercase(),
                    check_expr: "VALUE IS NOT NULL".to_owned(),
                });
                continue;
            }
            if consume_word_ci(sql, &mut cursor, "check").is_some() {
                skip_sql_whitespace(sql, &mut cursor);
                if let Some(check_body) = extract_parenthesized(sql, &mut cursor) {
                    constraints.push(DomainConstraint {
                        name: con_name.to_ascii_lowercase(),
                        check_expr: check_body,
                    });
                }
                continue;
            }
            break;
        }
        if consume_word_ci(sql, &mut cursor, "check").is_some() {
            skip_sql_whitespace(sql, &mut cursor);
            if let Some(check_body) = extract_parenthesized(sql, &mut cursor) {
                let auto_name = format!("{domain_name_lower}_check");
                constraints.push(DomainConstraint {
                    name: auto_name,
                    check_expr: check_body,
                });
            }
            continue;
        }
        cursor += sql[cursor..].chars().next().map_or(1, |c| c.len_utf8());
    }

    Some(DomainDef {
        name: domain_name_lower,
        schema_name: None,
        base_type,
        not_null,
        default_expr,
        constraints,
        char_length,
    })
}

/// Parse `DROP DOMAIN [IF EXISTS] name [CASCADE|RESTRICT]`
pub fn parse_drop_domain_tracking(statement_sql: &str) -> Option<String> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "domain")?;
    let saved = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some()
        && consume_word_ci(sql, &mut cursor, "exists").is_none()
    {
        cursor = saved;
    }
    let mut domain_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        domain_name = parse_identifier_part(sql, &mut cursor)?;
    }
    Some(domain_name.to_ascii_lowercase())
}

/// Parse `ALTER DOMAIN name ...`
pub fn parse_alter_domain_tracking(statement_sql: &str) -> Option<AlterDomainAction> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "domain")?;
    let mut domain_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        domain_name = parse_identifier_part(sql, &mut cursor)?;
    }
    let domain_name = domain_name.to_ascii_lowercase();
    skip_sql_whitespace(sql, &mut cursor);

    if consume_word_ci(sql, &mut cursor, "add").is_some() {
        if consume_word_ci(sql, &mut cursor, "constraint").is_some() {
            let con_name = parse_identifier_part(sql, &mut cursor)?;
            skip_sql_whitespace(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "not").is_some()
                && consume_word_ci(sql, &mut cursor, "null").is_some()
            {
                return Some(AlterDomainAction::AddConstraint {
                    domain_name,
                    constraint: DomainConstraint {
                        name: con_name.to_ascii_lowercase(),
                        check_expr: "VALUE IS NOT NULL".to_owned(),
                    },
                });
            }
            if consume_word_ci(sql, &mut cursor, "check").is_some() {
                skip_sql_whitespace(sql, &mut cursor);
                let check_body = extract_parenthesized(sql, &mut cursor)?;
                return Some(AlterDomainAction::AddConstraint {
                    domain_name,
                    constraint: DomainConstraint {
                        name: con_name.to_ascii_lowercase(),
                        check_expr: check_body,
                    },
                });
            }
        }
        if consume_word_ci(sql, &mut cursor, "check").is_some() {
            skip_sql_whitespace(sql, &mut cursor);
            let check_body = extract_parenthesized(sql, &mut cursor)?;
            let auto_name = format!("{domain_name}_check");
            return Some(AlterDomainAction::AddConstraint {
                domain_name,
                constraint: DomainConstraint {
                    name: auto_name,
                    check_expr: check_body,
                },
            });
        }
        return None;
    }
    if consume_word_ci(sql, &mut cursor, "drop").is_some() {
        if consume_word_ci(sql, &mut cursor, "constraint").is_some() {
            let saved = cursor;
            if consume_word_ci(sql, &mut cursor, "if").is_some()
                && consume_word_ci(sql, &mut cursor, "exists").is_none()
            {
                cursor = saved;
            }
            let con_name = parse_identifier_part(sql, &mut cursor)?;
            return Some(AlterDomainAction::DropConstraint {
                domain_name,
                constraint_name: con_name.to_ascii_lowercase(),
            });
        }
        if consume_word_ci(sql, &mut cursor, "not").is_some()
            && consume_word_ci(sql, &mut cursor, "null").is_some()
        {
            return Some(AlterDomainAction::DropNotNull { domain_name });
        }
        if consume_word_ci(sql, &mut cursor, "default").is_some() {
            return Some(AlterDomainAction::DropDefault { domain_name });
        }
        return None;
    }
    if consume_word_ci(sql, &mut cursor, "set").is_some() {
        if consume_word_ci(sql, &mut cursor, "not").is_some()
            && consume_word_ci(sql, &mut cursor, "null").is_some()
        {
            return Some(AlterDomainAction::SetNotNull { domain_name });
        }
        if consume_word_ci(sql, &mut cursor, "default").is_some() {
            skip_sql_whitespace(sql, &mut cursor);
            let rest = sql[cursor..].trim_end_matches(';').trim().to_owned();
            return Some(AlterDomainAction::SetDefault {
                domain_name,
                default_expr: rest,
            });
        }
        return None;
    }
    None
}

/// Map a user-written base type name to the normalised compat name.
fn map_base_type_name(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "INT" | "INT4" | "INTEGER" => "int4".to_owned(),
        "INT8" | "BIGINT" => "int8".to_owned(),
        "INT2" | "SMALLINT" => "int4".to_owned(),
        "REAL" | "FLOAT4" => "float4".to_owned(),
        "DOUBLE" | "DOUBLE PRECISION" | "FLOAT8" | "FLOAT" => "float8".to_owned(),
        "NUMERIC" | "DECIMAL" => "numeric".to_owned(),
        "TEXT" => "text".to_owned(),
        "VARCHAR" | "CHARACTER VARYING" => "text".to_owned(),
        "CHAR" | "CHARACTER" => "text".to_owned(),
        "BOOLEAN" | "BOOL" => "bool".to_owned(),
        "BYTEA" | "BLOB" => "bytea".to_owned(),
        "DATE" => "date".to_owned(),
        "TIMESTAMP" => "timestamp".to_owned(),
        "TIMESTAMPTZ" => "timestamptz".to_owned(),
        "TIME" => "time".to_owned(),
        "TIMETZ" => "timetz".to_owned(),
        "INTERVAL" => "interval".to_owned(),
        "UUID" => "uuid".to_owned(),
        "JSONB" | "JSON" => "jsonb".to_owned(),
        _ => normalize_compat_type_name(name),
    }
}

/// Try to peek at the next word without consuming it.
fn try_peek_word(sql: &str, pos: usize) -> Option<String> {
    let mut cursor = pos;
    skip_sql_whitespace(sql, &mut cursor);
    let start = cursor;
    while cursor < sql.len() {
        let ch = sql[cursor..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cursor += ch.len_utf8();
        } else {
            break;
        }
    }
    if cursor > start {
        Some(sql[start..cursor].to_owned())
    } else {
        None
    }
}

/// Check if a string starts with a keyword (case-insensitive, followed by
/// non-alphanumeric).
fn starts_with_keyword_ci(s: &str, keyword: &str) -> bool {
    if s.len() < keyword.len() {
        return false;
    }
    if !s[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return false;
    }
    s[keyword.len()..]
        .chars()
        .next()
        .map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
}
