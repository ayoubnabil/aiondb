//! Pure parsers that extract the information the engine needs from
//! PostgreSQL-compatible DDL variants the native parser does not yet
//! understand (ALTER INDEX, ALTER VIEW, ALTER TYPE ATTRIBUTE, CREATE/DROP
//! CAST, etc.). Each parser takes the original statement text and returns
//! a structured record; applying the effects is the engine's job.

use aiondb_eval::{normalize_compat_type_name, CompatCastContext};

use crate::scan::{
    consume_word_ci, extract_parenthesized, find_ascii_case_insensitive, parse_compat_bool,
    parse_compat_identifier, parse_compat_int, parse_compat_uint, parse_identifier_part,
    skip_sql_whitespace, trim_compat_statement,
};
use crate::type_ref::{
    parse_qualified_type_reference, parse_type_ref_list_until_rparen, parse_type_reference,
    ParsedCompatTypeRef,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatCast {
    pub source_type: String,
    pub target_type: String,
    pub context: CompatCastContext,
    pub method: ParsedCompatCastMethod,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatCastMethod {
    Binary,
    InOut,
    Function(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatDropCast {
    pub source_type: String,
    pub target_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatDropTypeOrDomain {
    pub schema_name: Option<String>,
    pub object_name: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatObjectName {
    pub schema_name: Option<String>,
    pub object_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatAlterRoleRename {
    pub source_name: String,
    pub target_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterIndexCommand {
    Rename {
        if_exists: bool,
        target: ParsedCompatObjectName,
        new_name: String,
    },
    AlterColumnSetStatistics {
        if_exists: bool,
        target: ParsedCompatObjectName,
        column_number: i32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterViewCommand {
    Rename {
        if_exists: bool,
        target: ParsedCompatObjectName,
        new_name: String,
    },
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterSetSchemaCommand {
    Table {
        if_exists: bool,
        target: ParsedCompatObjectName,
        new_schema: String,
    },
    Function {
        target: ParsedCompatObjectName,
        new_schema: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterTypeAttributeOperation {
    AddAttribute,
    AlterAttributeType,
    DropAttribute,
    RenameAttribute,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatAlterTypeAttributeCommand {
    pub target: ParsedCompatTypeRef,
    pub operation: ParsedCompatAlterTypeAttributeOperation,
    pub attribute_name: Option<String>,
    pub cascade: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterTableInheritCommand {
    Inherit {
        child: ParsedCompatObjectName,
        parent: ParsedCompatObjectName,
    },
    NoInherit {
        child: ParsedCompatObjectName,
        parent: ParsedCompatObjectName,
    },
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAttachPartitionBound {
    List { values: Vec<String> },
    Range { from: Vec<String>, to: Vec<String> },
    Hash { modulus: i64, remainder: i64 },
    Default,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedCompatAlterTablePartitionCommand {
    Attach {
        parent: ParsedCompatObjectName,
        child: ParsedCompatObjectName,
        bound: ParsedCompatAttachPartitionBound,
    },
    Detach {
        parent: ParsedCompatObjectName,
        child: ParsedCompatObjectName,
    },
}

pub enum ParsedCompatDatabaseCommand {
    Create { name: String },
    AlterRename { name: String, new_name: String },
    AlterSetTablespace { name: String, tablespace: String },
    AlterResetTablespace { name: String },
    AlterConnectionLimit { name: String, limit: Option<i32> },
    AlterOwner { name: String, owner: String },
    AlterAllowConnections { name: String, allow: bool },
    AlterIsTemplate { name: String, is_template: bool },
    AlterOther { name: String },
    Drop { name: String, if_exists: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedCompatInformationSchemaRoleTable {
    EnabledRoles,
    ApplicableRoles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCompatCreateRangeType {
    pub range_type_name: String,
    pub multirange_type_name: Option<String>,
}

/// High-level shape of a `CREATE TYPE` statement as recognised by the
/// compatibility layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateTypeKind {
    /// `CREATE TYPE name;` - shell.
    Shell,
    /// `CREATE TYPE name (INPUT = ..., OUTPUT = ...)` - base type.
    Base,
    /// `CREATE TYPE name AS (col type, ...)` - composite.
    Composite,
    /// `CREATE TYPE name AS ENUM (...)` - enum.
    Enum,
}

pub fn parse_compat_database_command(statement_sql: &str) -> Option<ParsedCompatDatabaseCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;

    if consume_word_ci(sql, &mut cursor, "create").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let name = parse_compat_identifier(sql, &mut cursor)?;
        return Some(ParsedCompatDatabaseCommand::Create { name });
    }

    cursor = 0;
    if consume_word_ci(sql, &mut cursor, "alter").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let name = parse_compat_identifier(sql, &mut cursor)?;
        if consume_word_ci(sql, &mut cursor, "rename").is_some() {
            consume_word_ci(sql, &mut cursor, "to")?;
            let new_name = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterRename { name, new_name });
        }
        if consume_word_ci(sql, &mut cursor, "set").is_some()
            && consume_word_ci(sql, &mut cursor, "tablespace").is_some()
        {
            let tablespace = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterSetTablespace { name, tablespace });
        }
        if consume_word_ci(sql, &mut cursor, "reset").is_some()
            && consume_word_ci(sql, &mut cursor, "tablespace").is_some()
        {
            return Some(ParsedCompatDatabaseCommand::AlterResetTablespace { name });
        }
        if consume_word_ci(sql, &mut cursor, "connection_limit").is_some() {
            let limit =
                parse_compat_uint(sql, &mut cursor).and_then(|value| i32::try_from(value).ok());
            return Some(ParsedCompatDatabaseCommand::AlterConnectionLimit { name, limit });
        }
        // PG-compat: `CONNECTION LIMIT n` (two-token spelling).
        let mut c_probe = cursor;
        if consume_word_ci(sql, &mut c_probe, "connection").is_some()
            && consume_word_ci(sql, &mut c_probe, "limit").is_some()
        {
            cursor = c_probe;
            let limit =
                parse_compat_uint(sql, &mut cursor).and_then(|value| i32::try_from(value).ok());
            return Some(ParsedCompatDatabaseCommand::AlterConnectionLimit { name, limit });
        }
        if consume_word_ci(sql, &mut cursor, "owner").is_some() {
            consume_word_ci(sql, &mut cursor, "to")?;
            let owner = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterOwner { name, owner });
        }
        if consume_word_ci(sql, &mut cursor, "allow_connections").is_some() {
            let allow = parse_compat_bool(sql, &mut cursor).unwrap_or(true);
            return Some(ParsedCompatDatabaseCommand::AlterAllowConnections { name, allow });
        }
        if consume_word_ci(sql, &mut cursor, "is_template").is_some() {
            let is_template = parse_compat_bool(sql, &mut cursor).unwrap_or(false);
            return Some(ParsedCompatDatabaseCommand::AlterIsTemplate { name, is_template });
        }
        return Some(ParsedCompatDatabaseCommand::AlterOther { name });
    }

    cursor = 0;
    if consume_word_ci(sql, &mut cursor, "drop").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let if_exists = consume_word_ci(sql, &mut cursor, "if").is_some()
            && consume_word_ci(sql, &mut cursor, "exists").is_some();
        let name = parse_compat_identifier(sql, &mut cursor)?;
        return Some(ParsedCompatDatabaseCommand::Drop { name, if_exists });
    }

    None
}

pub fn parse_compat_information_schema_role_table(
    statement_sql: &str,
) -> Option<ParsedCompatInformationSchemaRoleTable> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "table")?;
    let schema_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('.') {
        return None;
    }
    cursor += 1;
    let table_name = parse_compat_identifier(sql, &mut cursor)?;
    if !schema_name.eq_ignore_ascii_case("information_schema") {
        return None;
    }
    match table_name.as_str() {
        "enabled_roles" => Some(ParsedCompatInformationSchemaRoleTable::EnabledRoles),
        "applicable_roles" => Some(ParsedCompatInformationSchemaRoleTable::ApplicableRoles),
        _ => None,
    }
}

pub fn parse_create_type_name_and_kind(statement_sql: &str) -> Option<(String, CreateTypeKind)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut type_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        type_name = parse_compat_identifier(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let tail = sql.get(cursor..)?;
    if tail.is_empty() || tail.starts_with(';') {
        return Some((type_name, CreateTypeKind::Shell));
    }
    if tail.starts_with('(') {
        return Some((type_name, CreateTypeKind::Base));
    }
    if consume_word_ci(sql, &mut cursor, "as").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        if consume_word_ci(sql, &mut cursor, "range").is_some() {
            return None;
        }
        if consume_word_ci(sql, &mut cursor, "enum").is_some() {
            return Some((type_name, CreateTypeKind::Enum));
        }
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with('(') {
            return Some((type_name, CreateTypeKind::Composite));
        }
    }
    None
}

pub fn parse_compat_create_range_type(statement_sql: &str) -> Option<ParsedCompatCreateRangeType> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut range_type_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        range_type_name = parse_compat_identifier(sql, &mut cursor)?;
    }
    consume_word_ci(sql, &mut cursor, "as")?;
    consume_word_ci(sql, &mut cursor, "range")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    let options = extract_parenthesized(sql, &mut cursor)?;
    let multirange_type_name = parse_multirange_type_name_option(&options);
    Some(ParsedCompatCreateRangeType {
        range_type_name,
        multirange_type_name,
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
        if let Some(name) = parse_compat_identifier(part, &mut cursor) {
            return Some(name);
        }
    }
    None
}

pub fn parse_create_cast_statement(statement_sql: &str) -> Option<ParsedCompatCast> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source_type = parse_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target_type = parse_type_reference(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with(')') {
        return None;
    }
    cursor += 1;

    let method = if consume_word_ci(sql, &mut cursor, "without").is_some() {
        consume_word_ci(sql, &mut cursor, "function")?;
        ParsedCompatCastMethod::Binary
    } else if consume_word_ci(sql, &mut cursor, "with").is_some() {
        if consume_word_ci(sql, &mut cursor, "inout").is_some() {
            ParsedCompatCastMethod::InOut
        } else {
            consume_word_ci(sql, &mut cursor, "function")?;
            let function_name = parse_type_reference(sql, &mut cursor)?;
            skip_sql_whitespace(sql, &mut cursor);
            if !sql.get(cursor..)?.starts_with('(') {
                return None;
            }
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
            ParsedCompatCastMethod::Function(function_name)
        }
    } else {
        return None;
    };

    let context = if consume_word_ci(sql, &mut cursor, "as").is_some() {
        if consume_word_ci(sql, &mut cursor, "implicit").is_some() {
            CompatCastContext::Implicit
        } else if consume_word_ci(sql, &mut cursor, "assignment").is_some() {
            CompatCastContext::Assignment
        } else {
            return None;
        }
    } else {
        CompatCastContext::Explicit
    };

    Some(ParsedCompatCast {
        source_type: normalize_compat_type_name(&source_type),
        target_type: normalize_compat_type_name(&target_type),
        context,
        method,
    })
}

pub fn parse_drop_cast_statement(statement_sql: &str) -> Option<ParsedCompatDropCast> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    let _ = consume_word_ci(sql, &mut cursor, "if").and_then(|()| {
        consume_word_ci(sql, &mut cursor, "exists")?;
        Some(())
    });
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source_type = parse_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target_type = parse_type_reference(sql, &mut cursor)?;
    Some(ParsedCompatDropCast {
        source_type: normalize_compat_type_name(&source_type),
        target_type: normalize_compat_type_name(&target_type),
    })
}

pub fn parse_alter_role_rename_statement(
    statement_sql: &str,
) -> Option<ParsedCompatAlterRoleRename> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "role")?;
    let source_name = parse_identifier_part(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "rename")?;
    consume_word_ci(sql, &mut cursor, "to")?;
    let target_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(ParsedCompatAlterRoleRename {
        source_name,
        target_name,
    })
}

pub fn parse_compat_object_name(sql: &str, cursor: &mut usize) -> Option<ParsedCompatObjectName> {
    let first = parse_identifier_part(sql, cursor)?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..)?.starts_with('.') {
        *cursor += 1;
        let second = parse_identifier_part(sql, cursor)?;
        return Some(ParsedCompatObjectName {
            schema_name: Some(first),
            object_name: second,
        });
    }
    Some(ParsedCompatObjectName {
        schema_name: None,
        object_name: first,
    })
}

pub fn parse_compat_alter_index_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterIndexCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "index")?;

    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };

    let target = parse_compat_object_name(sql, &mut cursor)?;
    if consume_word_ci(sql, &mut cursor, "rename").is_some() {
        consume_word_ci(sql, &mut cursor, "to")?;
        let new_name = parse_identifier_part(sql, &mut cursor)?;
        skip_sql_whitespace(sql, &mut cursor);
        if cursor != sql.len() {
            return None;
        }
        return Some(ParsedCompatAlterIndexCommand::Rename {
            if_exists,
            target,
            new_name,
        });
    }

    consume_word_ci(sql, &mut cursor, "alter")?;
    let _ = consume_word_ci(sql, &mut cursor, "column");
    let column_number = parse_compat_int(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "set")?;
    consume_word_ci(sql, &mut cursor, "statistics")?;
    let _stats_target = parse_compat_int(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(ParsedCompatAlterIndexCommand::AlterColumnSetStatistics {
        if_exists,
        target,
        column_number,
    })
}

pub fn parse_compat_alter_view_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterViewCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "view")?;
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };
    let target = parse_compat_object_name(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "rename")?;
    consume_word_ci(sql, &mut cursor, "to")?;
    let new_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(ParsedCompatAlterViewCommand::Rename {
        if_exists,
        target,
        new_name,
    })
}

pub fn parse_compat_alter_type_attribute_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterTypeAttributeCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let target = parse_qualified_type_reference(sql, &mut cursor)?;

    let (operation, attribute_name) = if consume_word_ci(sql, &mut cursor, "add").is_some() {
        consume_word_ci(sql, &mut cursor, "attribute")?;
        let attribute_name = parse_identifier_part(sql, &mut cursor)?;
        let _attribute_type = parse_type_reference(sql, &mut cursor)?;
        (
            ParsedCompatAlterTypeAttributeOperation::AddAttribute,
            Some(attribute_name),
        )
    } else if consume_word_ci(sql, &mut cursor, "alter").is_some() {
        consume_word_ci(sql, &mut cursor, "attribute")?;
        let attribute_name = parse_identifier_part(sql, &mut cursor)?;
        consume_word_ci(sql, &mut cursor, "type")?;
        let _attribute_type = parse_type_reference(sql, &mut cursor)?;
        (
            ParsedCompatAlterTypeAttributeOperation::AlterAttributeType,
            Some(attribute_name),
        )
    } else if consume_word_ci(sql, &mut cursor, "drop").is_some() {
        consume_word_ci(sql, &mut cursor, "attribute")?;
        let attribute_name = parse_identifier_part(sql, &mut cursor)?;
        (
            ParsedCompatAlterTypeAttributeOperation::DropAttribute,
            Some(attribute_name),
        )
    } else if consume_word_ci(sql, &mut cursor, "rename").is_some() {
        consume_word_ci(sql, &mut cursor, "attribute")?;
        let attribute_name = parse_identifier_part(sql, &mut cursor)?;
        consume_word_ci(sql, &mut cursor, "to")?;
        let _new_attribute_name = parse_identifier_part(sql, &mut cursor)?;
        (
            ParsedCompatAlterTypeAttributeOperation::RenameAttribute,
            Some(attribute_name),
        )
    } else {
        return None;
    };

    let cascade = if consume_word_ci(sql, &mut cursor, "cascade").is_some() {
        true
    } else {
        let _ = consume_word_ci(sql, &mut cursor, "restrict");
        false
    };
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        let trailing = sql.get(cursor..)?.trim_start();
        if !trailing.is_empty() && !trailing.starts_with("--") {
            return None;
        }
    }
    Some(ParsedCompatAlterTypeAttributeCommand {
        target,
        operation,
        attribute_name,
        cascade,
    })
}

pub fn parse_compat_alter_table_inherit_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterTableInheritCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "table")?;
    let _ = consume_word_ci(sql, &mut cursor, "only");
    let child = parse_compat_object_name(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('*')) {
        cursor += 1;
    }

    let parsed = if consume_word_ci(sql, &mut cursor, "no").is_some() {
        consume_word_ci(sql, &mut cursor, "inherit")?;
        let parent = parse_compat_object_name(sql, &mut cursor)?;
        ParsedCompatAlterTableInheritCommand::NoInherit { child, parent }
    } else if consume_word_ci(sql, &mut cursor, "inherit").is_some() {
        let parent = parse_compat_object_name(sql, &mut cursor)?;
        ParsedCompatAlterTableInheritCommand::Inherit { child, parent }
    } else {
        return None;
    };

    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(parsed)
}

pub fn parse_compat_alter_table_target(statement_sql: &str) -> Option<ParsedCompatObjectName> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "table")?;
    let _ = consume_word_ci(sql, &mut cursor, "only");
    parse_compat_object_name(sql, &mut cursor)
}

#[allow(dead_code)]
pub fn parse_compat_alter_set_schema_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterSetSchemaCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    if consume_word_ci(sql, &mut cursor, "table").is_some() {
        let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
            consume_word_ci(sql, &mut cursor, "exists")?;
            true
        } else {
            false
        };
        let target = parse_compat_object_name(sql, &mut cursor)?;
        consume_word_ci(sql, &mut cursor, "set")?;
        consume_word_ci(sql, &mut cursor, "schema")?;
        let new_schema = parse_identifier_part(sql, &mut cursor)?;
        skip_sql_whitespace(sql, &mut cursor);
        if cursor != sql.len() {
            return None;
        }
        return Some(ParsedCompatAlterSetSchemaCommand::Table {
            if_exists,
            target,
            new_schema,
        });
    }

    consume_word_ci(sql, &mut cursor, "function")?;
    let target = parse_compat_object_name(sql, &mut cursor)?;
    let _signature = parse_type_ref_list_until_rparen(sql, &mut cursor, true)?;
    consume_word_ci(sql, &mut cursor, "set")?;
    consume_word_ci(sql, &mut cursor, "schema")?;
    let new_schema = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(ParsedCompatAlterSetSchemaCommand::Function { target, new_schema })
}

#[allow(dead_code)]
pub fn parse_compat_parenthesized_expr_items(sql: &str, cursor: &mut usize) -> Option<Vec<String>> {
    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..).is_some_and(|rest| rest.starts_with('(')) {
        return None;
    }
    *cursor += 1;

    let mut items = Vec::new();
    let mut item_start = *cursor;
    let mut pos = *cursor;
    let mut depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while pos < sql.len() {
        let ch = sql[pos..].chars().next()?;
        let next_pos = pos + ch.len_utf8();

        if in_single_quote {
            if ch == '\'' {
                if sql
                    .get(next_pos..)
                    .is_some_and(|rest| rest.starts_with('\''))
                {
                    pos = next_pos + 1;
                    continue;
                }
                in_single_quote = false;
            }
            pos = next_pos;
            continue;
        }

        if in_double_quote {
            if ch == '"' {
                if sql
                    .get(next_pos..)
                    .is_some_and(|rest| rest.starts_with('"'))
                {
                    pos = next_pos + 1;
                    continue;
                }
                in_double_quote = false;
            }
            pos = next_pos;
            continue;
        }

        match ch {
            '\'' => {
                in_single_quote = true;
            }
            '"' => {
                in_double_quote = true;
            }
            '(' => {
                depth = depth.saturating_add(1);
            }
            ')' => {
                if depth == 0 {
                    let item = sql.get(item_start..pos)?.trim();
                    if !item.is_empty() {
                        items.push(item.to_owned());
                    }
                    *cursor = next_pos;
                    return Some(items);
                }
                depth = depth.saturating_sub(1);
            }
            ',' if depth == 0 => {
                let item = sql.get(item_start..pos)?.trim();
                if !item.is_empty() {
                    items.push(item.to_owned());
                }
                item_start = next_pos;
            }
            _ => {}
        }

        pos = next_pos;
    }

    None
}

#[allow(dead_code)]
pub fn parse_compat_attach_partition_hash_bound(
    sql: &str,
    cursor: &mut usize,
) -> Option<(i64, i64)> {
    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..).is_some_and(|rest| rest.starts_with('(')) {
        return None;
    }
    *cursor += 1;

    consume_word_ci(sql, cursor, "modulus")?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..).is_some_and(|rest| rest.starts_with('=')) {
        *cursor += 1;
    }
    let modulus = i64::from(parse_compat_int(sql, cursor)?);

    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..).is_some_and(|rest| rest.starts_with(',')) {
        return None;
    }
    *cursor += 1;

    consume_word_ci(sql, cursor, "remainder")?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..).is_some_and(|rest| rest.starts_with('=')) {
        *cursor += 1;
    }
    let remainder = i64::from(parse_compat_int(sql, cursor)?);

    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
        return None;
    }
    *cursor += 1;

    Some((modulus, remainder))
}

#[allow(dead_code)]
pub fn parse_compat_alter_table_partition_command(
    statement_sql: &str,
) -> Option<ParsedCompatAlterTablePartitionCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "alter")?;
    consume_word_ci(sql, &mut cursor, "table")?;
    let _ = consume_word_ci(sql, &mut cursor, "only");
    let parent = parse_compat_object_name(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('*')) {
        cursor += 1;
    }

    if consume_word_ci(sql, &mut cursor, "attach").is_some() {
        consume_word_ci(sql, &mut cursor, "partition")?;
        let child = parse_compat_object_name(sql, &mut cursor)?;
        let bound = if consume_word_ci(sql, &mut cursor, "default").is_some() {
            ParsedCompatAttachPartitionBound::Default
        } else {
            consume_word_ci(sql, &mut cursor, "for")?;
            consume_word_ci(sql, &mut cursor, "values")?;
            if consume_word_ci(sql, &mut cursor, "in").is_some() {
                ParsedCompatAttachPartitionBound::List {
                    values: parse_compat_parenthesized_expr_items(sql, &mut cursor)?,
                }
            } else if consume_word_ci(sql, &mut cursor, "from").is_some() {
                let from = parse_compat_parenthesized_expr_items(sql, &mut cursor)?;
                consume_word_ci(sql, &mut cursor, "to")?;
                let to = parse_compat_parenthesized_expr_items(sql, &mut cursor)?;
                ParsedCompatAttachPartitionBound::Range { from, to }
            } else if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let (modulus, remainder) =
                    parse_compat_attach_partition_hash_bound(sql, &mut cursor)?;
                ParsedCompatAttachPartitionBound::Hash { modulus, remainder }
            } else {
                return None;
            }
        };
        skip_sql_whitespace(sql, &mut cursor);
        if cursor != sql.len() {
            return None;
        }
        return Some(ParsedCompatAlterTablePartitionCommand::Attach {
            parent,
            child,
            bound,
        });
    }

    if consume_word_ci(sql, &mut cursor, "detach").is_some() {
        consume_word_ci(sql, &mut cursor, "partition")?;
        let child = parse_compat_object_name(sql, &mut cursor)?;
        let _ = consume_word_ci(sql, &mut cursor, "concurrently")
            .or_else(|| consume_word_ci(sql, &mut cursor, "finalize"));
        skip_sql_whitespace(sql, &mut cursor);
        if cursor != sql.len() {
            return None;
        }
        return Some(ParsedCompatAlterTablePartitionCommand::Detach { parent, child });
    }

    None
}

pub fn parse_drop_type_or_domain_name(
    statement_sql: &str,
    target: &str,
) -> Option<ParsedCompatDropTypeOrDomain> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, target)?;
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };

    let mut schema_name = None;
    let mut object_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        schema_name = Some(object_name);
        cursor += 1;
        object_name = parse_identifier_part(sql, &mut cursor)?;
    }

    let tail = sql.get(cursor..).unwrap_or_default();
    let cascade = find_ascii_case_insensitive(tail, "cascade").is_some();
    Some(ParsedCompatDropTypeOrDomain {
        schema_name: schema_name.map(|name| name.to_ascii_lowercase()),
        object_name: object_name.to_ascii_lowercase(),
        if_exists,
        cascade,
    })
}
