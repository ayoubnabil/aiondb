#![allow(
    clippy::match_same_arms,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::used_underscore_binding,
    clippy::wildcard_imports
)]

use super::*;
use aiondb_eval::{normalize_compat_type_name, CompatUserTypeField, DomainConstraint, DomainDef};

pub(crate) fn statement_tracks_compat_types(statement: &aiondb_parser::Statement) -> bool {
    super::statement_compat_tag(statement).is_some_and(|tag| {
        matches!(
            tag,
            "CREATE TYPE" | "ALTER TYPE" | "CREATE DOMAIN" | "DROP DOMAIN" | "ALTER DOMAIN"
        )
    })
}

pub(crate) fn track_compat_types(
    record: &mut crate::session::SessionRecord,
    statement_sql: &str,
    statement: &aiondb_parser::Statement,
) {
    // Compute the compat tag from the statement variant so tagged compat
    // statements and first-class typed variants feed the same session trackers.
    let tag: &str = match statement {
        statement if super::statement_compat_tag(statement).is_some() => {
            super::statement_compat_tag(statement).unwrap_or_default()
        }
        _ => return,
    };

    match tag {
        "CREATE TYPE" => {
            let Some(parsed) = parse_create_type_tracking(statement_sql) else {
                return;
            };
            ensure_compat_user_type(record, &parsed.type_name);
            let normalized = normalize_compat_type_name(&parsed.type_name);
            let entry = std::sync::Arc::make_mut(&mut record.compat_user_types)
                .iter_mut()
                .find(|e| e.name == normalized);
            match parsed.kind {
                ParsedCreateTypeKind::Shell => {
                    record.shell_types.insert(parsed.type_name.clone());
                    if let Some(entry) = entry {
                        entry.schema_name = parsed.schema_name.clone();
                        entry.enum_labels.clear();
                        entry.composite_fields.clear();
                    }
                }
                ParsedCreateTypeKind::Enum(labels) => {
                    record.shell_types.remove(&parsed.type_name);
                    if let Some(entry) = entry {
                        entry.schema_name = parsed.schema_name.clone();
                        entry.enum_labels = labels;
                        entry.composite_fields.clear();
                    }
                }
                ParsedCreateTypeKind::Composite(fields) => {
                    record.shell_types.remove(&parsed.type_name);
                    if let Some(entry) = entry {
                        entry.schema_name = parsed.schema_name.clone();
                        entry.enum_labels.clear();
                        entry.composite_fields = fields;
                    }
                }
                ParsedCreateTypeKind::Range {
                    multirange_type_name,
                } => {
                    let normalized_range_name = normalize_compat_type_name(&parsed.type_name);
                    let normalized_multirange_name =
                        normalize_compat_type_name(&multirange_type_name);
                    record.shell_types.remove(&parsed.type_name);
                    record.shell_types.remove(&multirange_type_name);
                    if let Some(entry) = entry {
                        entry.schema_name = parsed.schema_name.clone();
                        entry.enum_labels.clear();
                        entry.composite_fields.clear();
                    }
                    ensure_compat_user_type(record, &multirange_type_name);
                    if let Some(multirange_entry) =
                        std::sync::Arc::make_mut(&mut record.compat_user_types)
                            .iter_mut()
                            .find(|e| e.name == normalized_multirange_name)
                    {
                        multirange_entry.schema_name = parsed.schema_name.clone();
                        multirange_entry.enum_labels.clear();
                        multirange_entry.composite_fields.clear();
                    }
                    std::sync::Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                        !(entry.source_type == normalized_range_name
                            && entry.target_type == normalized_multirange_name)
                    });
                    std::sync::Arc::make_mut(&mut record.compat_user_casts).push(
                        aiondb_eval::CompatUserCast {
                            oid: record.next_compat_cast_oid,
                            source_type: normalized_range_name,
                            target_type: normalized_multirange_name,
                            context: aiondb_eval::CompatCastContext::Explicit,
                            method: aiondb_eval::CompatCastMethod::InOut,
                        },
                    );
                    record.next_compat_cast_oid = record.next_compat_cast_oid.saturating_add(1);
                }
            }
        }
        "ALTER TYPE" => {
            if let Some(action) = parse_alter_type_action(statement_sql) {
                match action {
                    AlterTypeAction::AddEnumValue {
                        type_name,
                        new_label,
                        before,
                        after,
                    } => {
                        let normalized = normalize_compat_type_name(&type_name);
                        if let Some(entry) = std::sync::Arc::make_mut(&mut record.compat_user_types)
                            .iter_mut()
                            .find(|e| e.name == normalized)
                        {
                            if !entry.enum_labels.iter().any(|l| l == &new_label) {
                                let insert_at = if let Some(target) = before.as_ref() {
                                    entry.enum_labels.iter().position(|l| l == target)
                                } else if let Some(target) = after.as_ref() {
                                    entry
                                        .enum_labels
                                        .iter()
                                        .position(|l| l == target)
                                        .map(|idx| idx + 1)
                                } else {
                                    None
                                };
                                match insert_at {
                                    Some(idx) => entry.enum_labels.insert(idx, new_label),
                                    None => entry.enum_labels.push(new_label),
                                }
                            }
                        }
                    }
                    AlterTypeAction::RenameEnumValue {
                        type_name,
                        old_label,
                        new_label,
                    } => {
                        let normalized = normalize_compat_type_name(&type_name);
                        if let Some(entry) = std::sync::Arc::make_mut(&mut record.compat_user_types)
                            .iter_mut()
                            .find(|e| e.name == normalized)
                        {
                            if let Some(idx) =
                                entry.enum_labels.iter().position(|l| l == &old_label)
                            {
                                entry.enum_labels[idx] = new_label;
                            }
                        }
                    }
                }
            }
        }
        "CREATE DOMAIN" => {
            if let Some(domain_def) = parse_create_domain_tracking(statement_sql) {
                ensure_compat_user_type(record, &domain_def.name);
                // Remove any previous definition with the same name.
                let domain_defs = std::sync::Arc::make_mut(&mut record.domain_defs);
                domain_defs.retain(|d| d.name != domain_def.name);
                domain_defs.push(domain_def);
            }
        }
        "DROP DOMAIN" => {
            if let Some(domain_name) = parse_drop_domain_tracking(statement_sql) {
                std::sync::Arc::make_mut(&mut record.domain_defs).retain(|d| d.name != domain_name);
            }
        }
        "ALTER DOMAIN" => {
            if let Some(alter) = parse_alter_domain_tracking(statement_sql) {
                match alter {
                    AlterDomainAction::AddConstraint {
                        domain_name,
                        constraint,
                    } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.constraints.push(constraint);
                        }
                    }
                    AlterDomainAction::DropConstraint {
                        domain_name,
                        constraint_name,
                    } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.constraints.retain(|c| c.name != constraint_name);
                        }
                    }
                    AlterDomainAction::SetNotNull { domain_name } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.not_null = true;
                        }
                    }
                    AlterDomainAction::DropNotNull { domain_name } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.not_null = false;
                        }
                    }
                    AlterDomainAction::SetDefault {
                        domain_name,
                        default_expr,
                    } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.default_expr = Some(default_expr);
                        }
                    }
                    AlterDomainAction::DropDefault { domain_name } => {
                        if let Some(def) = std::sync::Arc::make_mut(&mut record.domain_defs)
                            .iter_mut()
                            .find(|d| d.name == domain_name)
                        {
                            def.default_expr = None;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedCreateTypeKind {
    Shell,
    Enum(Vec<String>),
    Composite(Vec<CompatUserTypeField>),
    Range { multirange_type_name: String },
}

/// `ALTER TYPE` actions we currently track at session+catalog level.
/// PG supports more (OWNER, SET SCHEMA, ADD ATTRIBUTE, RENAME ATTRIBUTE,
/// RENAME TO …); the unimplemented variants fall through to the
/// state.
#[derive(Clone, Debug, Eq, PartialEq)]
enum AlterTypeAction {
    AddEnumValue {
        type_name: String,
        new_label: String,
        before: Option<String>,
        after: Option<String>,
    },
    RenameEnumValue {
        type_name: String,
        old_label: String,
        new_label: String,
    },
}

fn parse_alter_type_action(statement_sql: &str) -> Option<AlterTypeAction> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut type_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|tail| tail.starts_with('.')) {
        cursor += 1;
        type_name = parse_identifier_part(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let mut action_probe = cursor;
    if consume_word_ci(sql, &mut action_probe, "add").is_some()
        && consume_word_ci(sql, &mut action_probe, "value").is_some()
    {
        // optional IF NOT EXISTS
        let mut probe_if = action_probe;
        if consume_word_ci(sql, &mut probe_if, "if").is_some()
            && consume_word_ci(sql, &mut probe_if, "not").is_some()
            && consume_word_ci(sql, &mut probe_if, "exists").is_some()
        {
            action_probe = probe_if;
        }
        let new_label = parse_quoted_string(sql, &mut action_probe)?;
        skip_sql_whitespace(sql, &mut action_probe);
        let mut before = None;
        let mut after = None;
        let mut probe_before = action_probe;
        if consume_word_ci(sql, &mut probe_before, "before").is_some() {
            before = parse_quoted_string(sql, &mut probe_before);
            if before.is_some() {
                action_probe = probe_before;
            }
        }
        let mut probe_after = action_probe;
        if consume_word_ci(sql, &mut probe_after, "after").is_some() {
            after = parse_quoted_string(sql, &mut probe_after);
            if after.is_some() {
                action_probe = probe_after;
            }
        }
        let _ = action_probe;
        return Some(AlterTypeAction::AddEnumValue {
            type_name,
            new_label,
            before,
            after,
        });
    }
    let mut rename_probe = cursor;
    if consume_word_ci(sql, &mut rename_probe, "rename").is_some()
        && consume_word_ci(sql, &mut rename_probe, "value").is_some()
    {
        let old_label = parse_quoted_string(sql, &mut rename_probe)?;
        skip_sql_whitespace(sql, &mut rename_probe);
        consume_word_ci(sql, &mut rename_probe, "to")?;
        let new_label = parse_quoted_string(sql, &mut rename_probe)?;
        return Some(AlterTypeAction::RenameEnumValue {
            type_name,
            old_label,
            new_label,
        });
    }
    None
}

/// Read a single-quoted SQL string literal and return its content with
/// `''` un-escaped. Returns `None` when the cursor isn't pointing at a
/// quoted string.
fn parse_quoted_string(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    let bytes = sql.as_bytes();
    if *cursor >= bytes.len() || bytes[*cursor] != b'\'' {
        return None;
    }
    let mut idx = *cursor + 1;
    let mut buf = String::new();
    while idx < bytes.len() {
        if bytes[idx] == b'\'' {
            if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                buf.push('\'');
                idx += 2;
                continue;
            }
            *cursor = idx + 1;
            return Some(buf);
        }
        buf.push(bytes[idx] as char);
        idx += 1;
    }
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCreateTypeTracking {
    schema_name: Option<String>,
    type_name: String,
    kind: ParsedCreateTypeKind,
}

fn parse_create_type_tracking(statement_sql: &str) -> Option<ParsedCreateTypeTracking> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut schema_name = None;
    let mut type_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql[cursor..].starts_with('.') {
        cursor += 1;
        schema_name = Some(type_name.to_ascii_lowercase());
        type_name = parse_identifier_part(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.trim_start().is_empty()
        || sql.get(cursor..)?.trim_start().starts_with(';')
    {
        return Some(ParsedCreateTypeTracking {
            schema_name,
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Shell,
        });
    }

    // `CREATE TYPE name (INPUT = ..., OUTPUT = ..., ...)` declares a
    // complete base type (no AS keyword). Treat it as a non-shell type so
    // subsequent function references and operators do not emit shell
    // notices.
    if sql.get(cursor..)?.starts_with('(') {
        return Some(ParsedCreateTypeTracking {
            schema_name,
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Composite(Vec::new()),
        });
    }

    consume_word_ci(sql, &mut cursor, "as")?;
    skip_sql_whitespace(sql, &mut cursor);
    if consume_word_ci(sql, &mut cursor, "range").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        if !sql.get(cursor..)?.starts_with('(') {
            return None;
        }
        let options = extract_parenthesized(sql, &mut cursor)?;
        let multirange_type_name = parse_multirange_type_name_option(&options)
            .unwrap_or_else(|| default_multirange_type_name(&type_name))
            .to_ascii_lowercase();
        return Some(ParsedCreateTypeTracking {
            schema_name,
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Range {
                multirange_type_name,
            },
        });
    }
    if consume_word_ci(sql, &mut cursor, "enum").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        let labels = parse_create_enum_labels_after_enum_keyword(sql, &mut cursor)?;
        return Some(ParsedCreateTypeTracking {
            schema_name,
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Enum(labels),
        });
    }
    if sql.get(cursor..)?.starts_with('(') {
        let inner = extract_parenthesized(sql, &mut cursor)?;
        let fields = parse_composite_type_fields(&inner)?;
        return Some(ParsedCreateTypeTracking {
            schema_name,
            type_name: type_name.to_ascii_lowercase(),
            kind: ParsedCreateTypeKind::Composite(fields),
        });
    }
    Some(ParsedCreateTypeTracking {
        schema_name,
        type_name: type_name.to_ascii_lowercase(),
        kind: ParsedCreateTypeKind::Shell,
    })
}

fn parse_multirange_type_name_option(options: &str) -> Option<String> {
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
        if let Some(name) = parse_identifier_part(part, &mut cursor) {
            return Some(name);
        }
    }
    None
}

fn default_multirange_type_name(range_type_name: &str) -> String {
    let normalized = normalize_compat_type_name(range_type_name);
    if let Some(prefix) = normalized.strip_suffix("range") {
        format!("{prefix}multirange")
    } else {
        format!("{normalized}_multirange")
    }
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
    if table.columns.is_empty() {
        return None;
    }
    Some(
        table
            .columns
            .into_iter()
            .map(|column| CompatUserTypeField {
                name: column.name,
                raw_type_name: Some(normalize_compat_type_name(&column.data_type.to_string())),
                data_type: column.data_type,
            })
            .collect(),
    )
}

/// Parse `CREATE DOMAIN name [AS] base_type [NOT NULL] [DEFAULT ...] [CHECK (...)]`
fn parse_create_domain_tracking(statement_sql: &str) -> Option<DomainDef> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "domain")?;
    let mut domain_name = parse_identifier_part(sql, &mut cursor)?;
    let mut schema_name = None;
    skip_sql_whitespace(sql, &mut cursor);
    // Handle optional schema qualification
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        schema_name = Some(domain_name.to_ascii_lowercase());
        domain_name = parse_identifier_part(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    // Optional AS keyword
    let _ = consume_word_ci(sql, &mut cursor, "as");
    skip_sql_whitespace(sql, &mut cursor);
    // Parse base type name -- may be multi-word (e.g. "character varying")
    let _base_type_start = cursor;
    let base_type_ident = parse_identifier_part(sql, &mut cursor)?;
    // Check for multi-word types like "character varying", "double precision"
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
    // Skip optional type modifiers like (5) or (8,2)
    skip_sql_whitespace(sql, &mut cursor);
    let mut char_length: Option<u32> = None;
    if sql.get(cursor..)?.starts_with('(') {
        cursor += 1; // consume (
        skip_sql_whitespace(sql, &mut cursor);
        // Try to parse the length number
        let num_start = cursor;
        while cursor < sql.len() && sql[cursor..].starts_with(|c: char| c.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor > num_start {
            let num_str = &sql[num_start..cursor];
            if let Ok(n) = num_str.parse::<u32>() {
                // Only store char_length for varchar/char types
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
        // Skip to closing paren
        while cursor < sql.len() && !sql[cursor..].starts_with(')') {
            cursor += 1;
        }
        if cursor < sql.len() {
            cursor += 1; // consume )
        }
    }
    // Detect optional array dimensions like [1] or [][3]
    skip_sql_whitespace(sql, &mut cursor);
    let mut is_array = false;
    while sql.get(cursor..)?.starts_with('[') {
        is_array = true;
        while cursor < sql.len() && !sql[cursor..].starts_with(']') {
            cursor += 1;
        }
        if cursor < sql.len() {
            cursor += 1; // consume ]
        }
        skip_sql_whitespace(sql, &mut cursor);
    }

    // Map the base type to the normalized compat type name
    let mut base_type = map_base_type_name(&base_type_str);
    if is_array {
        base_type.push_str("[]");
    }

    let mut not_null = false;
    let mut default_expr = None;
    let mut constraints = Vec::new();
    let domain_name_lower = domain_name.to_ascii_lowercase();

    // Parse remaining clauses
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
            // Consume until CHECK, CONSTRAINT, NOT, or end
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
                // Also store as a CHECK constraint for error message compatibility
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
            // Unknown constraint type, skip
            break;
        }
        if consume_word_ci(sql, &mut cursor, "check").is_some() {
            skip_sql_whitespace(sql, &mut cursor);
            if let Some(check_body) = extract_parenthesized(sql, &mut cursor) {
                // Auto-generate a constraint name
                let auto_name = format!("{domain_name_lower}_check");
                constraints.push(DomainConstraint {
                    name: auto_name,
                    check_expr: check_body,
                });
            }
            continue;
        }
        // Unknown token, skip one char and try again
        cursor += sql[cursor..].chars().next().map_or(1, |c| c.len_utf8());
    }

    Some(DomainDef {
        name: domain_name_lower,
        schema_name,
        base_type,
        not_null,
        default_expr,
        constraints,
        char_length,
    })
}

/// Parse `DROP DOMAIN [IF EXISTS] name [CASCADE|RESTRICT]`
fn parse_drop_domain_tracking(statement_sql: &str) -> Option<String> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "domain")?;
    // Optional IF EXISTS
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

enum AlterDomainAction {
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

/// Parse `ALTER DOMAIN name ...`
fn parse_alter_domain_tracking(statement_sql: &str) -> Option<AlterDomainAction> {
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
