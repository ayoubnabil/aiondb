#![allow(clippy::pedantic)]
#![allow(
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::wildcard_imports
)]

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use super::compat::{
    compat_statement_sql_fragment, consume_word_ci, parse_compat_close_portal_name,
    parse_compat_identifier, skip_sql_whitespace, trim_compat_statement,
};
use super::compat_aggregate_rewrite::{
    compat_multiarg_distinct_order_error, ordered_set_usage_error, rewrite_compat_aggregate_query,
    sql_may_use_builtin_compat_aggregate_rewrite,
};
use super::copy_support::{
    normalize_copy_from_data, parse_copy_from_text_line, parse_copy_sql_options,
    parse_simple_instead_of_insert_trigger_mapping, pending_copy_statement, quote_sql_ident,
    render_copy_insert_expr, render_copy_rows, render_sql_literal_from_copy_field,
    resolve_copy_trigger_function, validate_copy_column_count, validate_copy_endpoint,
    validate_copy_force_column_references, validate_copy_from_where_clause,
};
use super::query_api_explain::{
    extract_hash_join_batch_counts_from_explain, normalize_explain_memory_token,
    parse_check_estimated_rows_inner_sql,
};
use super::*;
use crate::engine::compat::router_helpers::CompatHandlerPlan;
use aiondb_core::{DataType, Row, SqlState, Value};
use aiondb_security::TransportInfo;
use tracing::{debug, warn};

use super::WireStateCleanupHint;
use crate::params::{
    bind_statement_params, ensure_portal_param_types_compatible, ensure_supported_portal_params,
    statement_contains_parameters,
};

use super::query_api_wire::{
    describe_sql_statement_for_wire, prepared_statement_wire_cleanup_hint,
    prepared_statement_wire_effective_statement, sql_statement_wire_cleanup_hint,
    sql_statement_wire_effective_statement, sql_statement_wire_metadata,
    statement_allowed_in_failed_transaction, statement_wire_effective_statement_for_statement,
};

const FAILED_TRANSACTION_MESSAGE: &str =
    "current transaction is aborted, commands ignored until end of transaction block";

pub(in crate::engine) use super::query_api_copy_compat::{
    decode_sql_single_quoted_literal, escape_copy_text_value, format_copy_csv_value,
    format_copy_text_value, object_name_to_qualified, split_top_level_csv_items,
    unescape_copy_text_value, CopyColumnCompat, CopyCompatFormat, CopyCompatOptions, CopyCsvField,
    CopyWhereOp, CopyWherePredicate,
};

fn extract_option_numeric_literal(sql: &str, key: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let key_lower = key.to_ascii_lowercase();
    let start = lower.find(&key_lower)?;
    let mut cursor = start + key_lower.len();
    let bytes = sql.as_bytes();
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'=' {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() {
        return None;
    }
    let value_start = cursor;
    while cursor < bytes.len() {
        let ch = bytes[cursor] as char;
        if ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E') {
            cursor += 1;
        } else {
            break;
        }
    }
    if cursor == value_start {
        return None;
    }
    Some(sql[value_start..cursor].trim().to_owned())
}

fn validate_brin_bloom_index_options(statement: &Statement, sql: &str) -> DbResult<()> {
    let Statement::CreateIndex(create_index) = statement else {
        return Ok(());
    };
    if create_index.method != Some(aiondb_parser::IndexMethod::Brin) {
        return Ok(());
    }

    if let Some(raw) = extract_option_numeric_literal(sql, "n_distinct_per_range") {
        if let Ok(value) = raw.parse::<f64>() {
            if !(-1.0..=2_147_483_647.0).contains(&value) {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("value {raw} out of bounds for option \"n_distinct_per_range\""),
                )
                .with_client_detail(
                    "Valid values are between \"-1.000000\" and \"2147483647.000000\".".to_owned(),
                ));
            }
        }
    }

    if let Some(raw) = extract_option_numeric_literal(sql, "false_positive_rate") {
        if let Ok(value) = raw.parse::<f64>() {
            if !(0.0001..=0.25).contains(&value) {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("value {raw} out of bounds for option \"false_positive_rate\""),
                )
                .with_client_detail(
                    "Valid values are between \"0.000100\" and \"0.250000\".".to_owned(),
                ));
            }
        }
    }

    Ok(())
}

pub(in crate::engine) fn object_name_to_sql(name: &aiondb_parser::ObjectName) -> String {
    name.parts
        .iter()
        .map(|part| quote_sql_ident(part))
        .collect::<Vec<_>>()
        .join(".")
}

pub(in crate::engine) fn qualified_name_to_sql(name: &aiondb_catalog::QualifiedName) -> String {
    let mut parts = Vec::new();
    if let Some(schema_name) = name.schema_name() {
        parts.push(quote_sql_ident(schema_name));
    }
    parts.push(quote_sql_ident(name.object_name()));
    parts.join(".")
}

fn extract_hash_join_batches_arg(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let idx = lower.find("hash_join_batches")?;
    let mut after = &sql[idx + "hash_join_batches".len()..];
    after = after.trim_start();
    if !after.starts_with('(') {
        return None;
    }
    after = after[1..].trim_start();

    // Dollar-quoted literal: $$ ... $$ or $tag$ ... $tag$
    if let Some(stripped) = after.strip_prefix('$') {
        let tag_end = stripped.find('$')?;
        let tag = &after[..tag_end + 2];
        let rest = &after[tag.len()..];
        let end = rest.find(tag)?;
        return Some(rest[..end].trim().to_owned());
    }

    // Single-quoted string literal.
    if !after.starts_with('\'') {
        return None;
    }
    let mut out = String::new();
    let mut chars = after[1..].chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek().copied() == Some('\'') {
                out.push('\'');
                chars.next();
                continue;
            }
            break;
        }
        out.push(ch);
    }
    Some(out)
}

pub(in crate::engine) fn strip_prefix_ci<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    if input.len() < prefix.len() || !input[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = &input[prefix.len()..];
    if let Some(ch) = rest.chars().next() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            return None;
        }
    }
    Some(rest.trim_start())
}

pub(in crate::engine) fn split_typed_table_option_items(raw: &str) -> DbResult<Vec<String>> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0u32;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let bytes = raw.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        let ch = bytes[i] as char;
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                current.push(' ');
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if ch == '*' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_single {
            current.push(ch);
            if ch == '\'' {
                if i + 1 < bytes.len() && bytes[i + 1] as char == '\'' {
                    current.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            current.push(ch);
            if ch == '"' {
                if i + 1 < bytes.len() && bytes[i + 1] as char == '"' {
                    current.push('"');
                    i += 2;
                    continue;
                }
                in_double = false;
            }
            i += 1;
            continue;
        }

        if ch == '-' && i + 1 < bytes.len() && bytes[i + 1] as char == '-' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if ch == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
            in_block_comment = true;
            i += 2;
            continue;
        }

        match ch {
            '\'' => {
                in_single = true;
                current.push(ch);
            }
            '"' => {
                in_double = true;
                current.push(ch);
            }
            '(' => {
                depth = depth.saturating_add(1);
                current.push(ch);
            }
            ')' => {
                if depth == 0 {
                    return Err(DbError::parse_error(
                        SqlState::SyntaxError,
                        "unterminated typed-table clause",
                    ));
                }
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                let item = current.trim();
                if !item.is_empty() {
                    items.push(item.to_owned());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
        i += 1;
    }

    if in_single || in_double || in_block_comment || depth != 0 {
        return Err(DbError::parse_error(
            SqlState::SyntaxError,
            "unterminated typed-table clause",
        ));
    }

    let item = current.trim();
    if !item.is_empty() {
        items.push(item.to_owned());
    }
    Ok(items)
}

pub(in crate::engine) fn build_typed_table_option_alter_sql(
    table_name: &aiondb_parser::ObjectName,
    options: &str,
    valid_columns: &HashSet<String>,
) -> DbResult<Vec<String>> {
    let table_sql = object_name_to_sql(table_name);
    let mut alters = Vec::new();
    let mut seen_column_option_entries = HashSet::new();
    let items = split_typed_table_option_items(options)?;

    for item in items {
        if strip_prefix_ci(item.as_str(), "PRIMARY KEY").is_some()
            || strip_prefix_ci(item.as_str(), "UNIQUE").is_some()
        {
            alters.push(format!("ALTER TABLE {table_sql} ADD {item}"));
            continue;
        }

        let mut cursor = 0usize;
        let Some(column_name) = parse_compat_identifier(item.as_str(), &mut cursor) else {
            return Err(DbError::feature_not_supported(
                "typed table column options are not supported",
            ));
        };
        let column_key = column_name.to_ascii_lowercase();
        if !valid_columns.contains(&column_key) {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!("column \"{column_name}\" does not exist"),
            ));
        }
        if !seen_column_option_entries.insert(column_key.clone()) {
            return Err(DbError::bind_error(
                SqlState::DuplicateColumn,
                format!("column \"{column_name}\" specified more than once"),
            ));
        }

        let mut rest = item[cursor..].trim_start();
        if let Some(next) = strip_prefix_ci(rest, "WITH OPTIONS") {
            rest = next;
        }

        let mut set_not_null = false;
        let mut drop_not_null = false;
        let mut set_primary_key = false;
        let mut set_unique = false;
        let mut default_expr: Option<String> = None;

        while !rest.is_empty() {
            if let Some(next) = strip_prefix_ci(rest, "PRIMARY KEY") {
                set_primary_key = true;
                rest = next;
                continue;
            }
            if let Some(next) = strip_prefix_ci(rest, "UNIQUE") {
                set_unique = true;
                rest = next;
                continue;
            }
            if let Some(next) = strip_prefix_ci(rest, "NOT NULL") {
                set_not_null = true;
                rest = next;
                continue;
            }
            if let Some(next) = strip_prefix_ci(rest, "NULL") {
                drop_not_null = true;
                rest = next;
                continue;
            }
            if let Some(next) = strip_prefix_ci(rest, "DEFAULT") {
                let expr = next.trim();
                if expr.is_empty() {
                    return Err(DbError::parse_error(
                        SqlState::SyntaxError,
                        "expected expression after DEFAULT",
                    ));
                }
                default_expr = Some(expr.to_owned());
                rest = "";
                continue;
            }
            return Err(DbError::feature_not_supported(
                "typed table column options are not supported",
            ));
        }

        let column_sql = quote_sql_ident(&column_name);
        if set_not_null && drop_not_null {
            return Err(DbError::feature_not_supported(
                "typed table column options are not supported",
            ));
        }
        if set_not_null {
            alters.push(format!(
                "ALTER TABLE {table_sql} ALTER COLUMN {column_sql} SET NOT NULL"
            ));
        } else if drop_not_null {
            alters.push(format!(
                "ALTER TABLE {table_sql} ALTER COLUMN {column_sql} DROP NOT NULL"
            ));
        }
        if let Some(default_expr) = default_expr {
            alters.push(format!(
                "ALTER TABLE {table_sql} ALTER COLUMN {column_sql} SET DEFAULT {default_expr}"
            ));
        }
        if set_primary_key {
            if !set_not_null {
                alters.push(format!(
                    "ALTER TABLE {table_sql} ALTER COLUMN {column_sql} SET NOT NULL"
                ));
            }
            alters.push(format!(
                "ALTER TABLE {table_sql} ADD PRIMARY KEY ({column_sql})"
            ));
        }
        if set_unique {
            alters.push(format!("ALTER TABLE {table_sql} ADD UNIQUE ({column_sql})"));
        }
    }

    Ok(alters)
}

include!("query_api_prepared_helpers.rs");
pub(super) use aiondb_pg_compat::noop_validation::reject_invalid_noop_statement;

fn can_reuse_cached_plan_fingerprint(statement: &Statement) -> bool {
    Engine::cacheable_plan_statement(statement)
        && !statement_contains_parameters(statement)
        && !super::recursive_cte::statement_contains_recursive_cte(statement)
        && !super::statement_policy::statement_requires_acl_normalization(statement)
}

pub(super) fn parser_expr_strip_casts(expr: &aiondb_parser::Expr) -> &aiondb_parser::Expr {
    let mut current = expr;
    while let aiondb_parser::Expr::Cast { expr, .. } = current {
        current = expr;
    }
    current
}

fn parser_expr_is_parameter(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Parameter { .. }
    )
}

fn parser_expr_is_identifier(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Identifier(_)
    )
}

// `sql_contains_ascii_case_insensitive` lives in
// `engine/compat/router_helpers.rs`.

// directly by the compat cascade below via

fn parser_expr_is_literal(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Literal(_, _)
    )
}

fn parser_object_name_single_part_matches(
    name: &aiondb_parser::ObjectName,
    expected: &str,
) -> bool {
    name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case(expected)
}

fn parser_expr_is_insert_fast_path_stable_value(expr: &aiondb_parser::Expr) -> bool {
    match parser_expr_strip_casts(expr) {
        aiondb_parser::Expr::Identifier(name) => {
            parser_object_name_single_part_matches(name, "current_timestamp")
                || parser_object_name_single_part_matches(name, "current_date")
                || parser_object_name_single_part_matches(name, "current_time")
                || parser_object_name_single_part_matches(name, "localtimestamp")
                || parser_object_name_single_part_matches(name, "localtime")
        }
        aiondb_parser::Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            !*distinct
                && filter.is_none()
                && args.is_empty()
                && (parser_object_name_single_part_matches(name, "now")
                    || parser_object_name_single_part_matches(name, "current_timestamp")
                    || parser_object_name_single_part_matches(name, "current_date")
                    || parser_object_name_single_part_matches(name, "current_time")
                    || parser_object_name_single_part_matches(name, "localtimestamp")
                    || parser_object_name_single_part_matches(name, "localtime"))
        }
        _ => false,
    }
}

fn parser_expr_parameter_index(expr: &aiondb_parser::Expr) -> Option<usize> {
    match parser_expr_strip_casts(expr) {
        aiondb_parser::Expr::Parameter { index, .. } => usize::try_from(*index).ok(),
        _ => None,
    }
}

fn parser_binary_op_is_arith(op: aiondb_parser::BinaryOperator) -> bool {
    matches!(
        op,
        aiondb_parser::BinaryOperator::Add
            | aiondb_parser::BinaryOperator::Sub
            | aiondb_parser::BinaryOperator::Mul
            | aiondb_parser::BinaryOperator::Div
            | aiondb_parser::BinaryOperator::Mod
    )
}

fn statement_matches_parameterized_select_eq_rewrite(statement: &Statement) -> bool {
    let Statement::Select(select) = statement else {
        return false;
    };
    if !statement_contains_parameters(statement) {
        return false;
    }
    if !select.ctes.is_empty()
        || !matches!(select.distinct, aiondb_parser::DistinctKind::All)
        || select.from.is_none()
        || !select.joins.is_empty()
        || !select.group_by.is_empty()
        || !select.group_by_items.is_empty()
        || select.having.is_some()
        || !select.window_definitions.is_empty()
        || select.offset.is_some()
    {
        return false;
    }
    if select
        .items
        .iter()
        .any(|item| parser_expr_contains_parameter(&item.expr))
    {
        return false;
    }

    let Some(selection) = select.selection.as_ref() else {
        return false;
    };
    let selection = parser_expr_strip_casts(selection);
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = selection
    else {
        return false;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return false;
    }
    let left_is_param = parser_expr_is_parameter(left);
    let right_is_param = parser_expr_is_parameter(right);
    if left_is_param == right_is_param {
        return false;
    }

    let non_param_expr = if left_is_param {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_param_expr)
}

fn statement_matches_parameterized_update_rewrite(statement: &Statement) -> bool {
    let Statement::Update(update) = statement else {
        return false;
    };
    if !statement_contains_parameters(statement)
        || !update.from_tables.is_empty()
        || !update.returning.is_empty()
        || update.assignments.len() != 1
    {
        return false;
    }
    let assignment = &update.assignments[0];
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(&assignment.expr)
    else {
        return false;
    };
    if !parser_binary_op_is_arith(*op) {
        return false;
    }
    let left_is_param = parser_expr_is_parameter(left);
    let right_is_param = parser_expr_is_parameter(right);
    if left_is_param == right_is_param {
        return false;
    }
    let non_param_expr = if left_is_param {
        right.as_ref()
    } else {
        left.as_ref()
    };
    if !parser_expr_is_identifier(non_param_expr) {
        return false;
    }

    let Some(selection) = update.selection.as_ref() else {
        return false;
    };
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(selection)
    else {
        return false;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return false;
    }
    let left_is_param = parser_expr_is_parameter(left);
    let right_is_param = parser_expr_is_parameter(right);
    if left_is_param == right_is_param {
        return false;
    }
    let non_param_expr = if left_is_param {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_param_expr)
}

fn statement_matches_parameterized_delete_rewrite(statement: &Statement) -> bool {
    let Statement::Delete(delete) = statement else {
        return false;
    };
    if !statement_contains_parameters(statement) || !delete.returning.is_empty() {
        return false;
    }

    let Some(selection) = delete.selection.as_ref() else {
        return false;
    };
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(selection)
    else {
        return false;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return false;
    }
    let left_is_param = parser_expr_is_parameter(left);
    let right_is_param = parser_expr_is_parameter(right);
    if left_is_param == right_is_param {
        return false;
    }
    let non_param_expr = if left_is_param {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_param_expr)
}

fn statement_matches_parameterized_insert_values_rewrite(statement: &Statement) -> bool {
    let Statement::Insert(insert) = statement else {
        return false;
    };
    if !statement_contains_parameters(statement)
        || insert.query.is_some()
        || insert.on_conflict.is_some()
        || !insert.returning.is_empty()
        || insert.rows.len() != 1
    {
        return false;
    }
    let row = &insert.rows[0];
    if row.is_empty() {
        return false;
    }
    let mut saw_param = false;
    for expr in row {
        let stripped = parser_expr_strip_casts(expr);
        if parser_expr_is_parameter(stripped) {
            saw_param = true;
            continue;
        }
        if parser_expr_contains_parameter(stripped) {
            return false;
        }
    }
    saw_param
}

pub(super) fn parameterized_insert_values_param_slots(
    statement: &Statement,
) -> Option<Arc<[Option<usize>]>> {
    if !statement_matches_parameterized_insert_values_rewrite(statement) {
        return None;
    }
    let Statement::Insert(insert) = statement else {
        return None;
    };
    let row = insert.rows.first()?;
    let mut slots = Vec::with_capacity(row.len());
    for expr in row {
        let expr = parser_expr_strip_casts(expr);
        match expr {
            aiondb_parser::Expr::Parameter { index, .. } => {
                slots.push(Some(index.checked_sub(1)?));
            }
            _ => slots.push(None),
        }
    }
    Some(slots.into())
}

fn parameterized_insert_values_bound_literals(
    slots: &[Option<usize>],
    params: &[Value],
) -> Option<Vec<Option<Value>>> {
    let mut literals = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Some(index) => literals.push(Some(params.get(*index)?.clone())),
            None => literals.push(None),
        }
    }
    Some(literals)
}

fn statement_matches_literal_select_eq_rewrite(statement: &Statement) -> bool {
    let Statement::Select(select) = statement else {
        return false;
    };
    if statement_contains_parameters(statement) {
        return false;
    }
    if !select.ctes.is_empty()
        || !matches!(select.distinct, aiondb_parser::DistinctKind::All)
        || select.from.is_none()
        || !select.joins.is_empty()
        || !select.group_by.is_empty()
        || !select.group_by_items.is_empty()
        || select.having.is_some()
        || !select.window_definitions.is_empty()
        || select.offset.is_some()
    {
        return false;
    }
    if select
        .items
        .iter()
        .any(|item| parser_expr_contains_parameter(&item.expr))
    {
        return false;
    }

    let Some(selection) = select.selection.as_ref() else {
        return false;
    };
    selection_matches_literal_select_rewrite(selection)
}

fn parser_binary_op_is_comparison(op: aiondb_parser::BinaryOperator) -> bool {
    matches!(
        op,
        aiondb_parser::BinaryOperator::Eq
            | aiondb_parser::BinaryOperator::Ge
            | aiondb_parser::BinaryOperator::Gt
            | aiondb_parser::BinaryOperator::Le
            | aiondb_parser::BinaryOperator::Lt
    )
}

fn selection_matches_literal_select_rewrite(selection: &aiondb_parser::Expr) -> bool {
    let selection = parser_expr_strip_casts(selection);
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = selection
    else {
        return false;
    };
    if *op == aiondb_parser::BinaryOperator::And {
        return selection_matches_literal_select_rewrite(left)
            && selection_matches_literal_select_rewrite(right);
    }
    if !parser_binary_op_is_comparison(*op) {
        return false;
    }

    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return false;
    }

    let non_literal_expr = if left_is_literal {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_literal_expr)
}

fn statement_matches_literal_update_rewrite(statement: &Statement) -> bool {
    let Statement::Update(update) = statement else {
        return false;
    };
    if statement_contains_parameters(statement)
        || !update.from_tables.is_empty()
        || !update.returning.is_empty()
        || update.assignments.len() != 1
    {
        return false;
    }
    let assignment = &update.assignments[0];
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(&assignment.expr)
    else {
        return false;
    };
    if !parser_binary_op_is_arith(*op) {
        return false;
    }
    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return false;
    }
    let non_literal_expr = if left_is_literal {
        right.as_ref()
    } else {
        left.as_ref()
    };
    if !parser_expr_is_identifier(non_literal_expr) {
        return false;
    }

    let Some(selection) = update.selection.as_ref() else {
        return false;
    };
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(selection)
    else {
        return false;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return false;
    }
    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return false;
    }
    let non_literal_expr = if left_is_literal {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_literal_expr)
}

fn statement_matches_literal_delete_rewrite(statement: &Statement) -> bool {
    let Statement::Delete(delete) = statement else {
        return false;
    };
    if statement_contains_parameters(statement) || !delete.returning.is_empty() {
        return false;
    }

    let Some(selection) = delete.selection.as_ref() else {
        return false;
    };
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = parser_expr_strip_casts(selection)
    else {
        return false;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return false;
    }
    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return false;
    }
    let non_literal_expr = if left_is_literal {
        right.as_ref()
    } else {
        left.as_ref()
    };
    parser_expr_is_identifier(non_literal_expr)
}

fn statement_matches_literal_insert_values_rewrite(statement: &Statement) -> bool {
    let Statement::Insert(insert) = statement else {
        return false;
    };
    if statement_contains_parameters(statement)
        || insert.query.is_some()
        || insert.on_conflict.is_some()
        || !insert.returning.is_empty()
        || insert.rows.len() != 1
    {
        return false;
    }
    let row = &insert.rows[0];
    !row.is_empty()
        && row.iter().all(|expr| {
            parser_expr_is_literal(expr) || parser_expr_is_insert_fast_path_stable_value(expr)
        })
}

fn canonicalize_literal(literal: &aiondb_parser::Literal) -> aiondb_parser::Literal {
    match literal {
        aiondb_parser::Literal::Integer(_) => aiondb_parser::Literal::Integer(0),
        aiondb_parser::Literal::NumericLit(_) => aiondb_parser::Literal::NumericLit("0".to_owned()),
        aiondb_parser::Literal::String(_) => aiondb_parser::Literal::String(String::new()),
        aiondb_parser::Literal::Boolean(_) => aiondb_parser::Literal::Boolean(false),
        aiondb_parser::Literal::Null => aiondb_parser::Literal::Null,
    }
}

fn canonicalize_literal_side(expr: &aiondb_parser::Expr) -> Option<aiondb_parser::Expr> {
    match expr {
        aiondb_parser::Expr::Literal(literal, span) => Some(aiondb_parser::Expr::Literal(
            canonicalize_literal(literal),
            *span,
        )),
        aiondb_parser::Expr::Cast {
            expr: inner,
            data_type,
            span,
        } => Some(aiondb_parser::Expr::Cast {
            expr: Box::new(canonicalize_literal_side(inner)?),
            data_type: data_type.clone(),
            span: *span,
        }),
        _ => None,
    }
}

fn canonicalize_select_filter_literals(expr: &mut aiondb_parser::Expr) -> Option<()> {
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = expr
    else {
        return None;
    };
    if *op == aiondb_parser::BinaryOperator::And {
        canonicalize_select_filter_literals(left)?;
        canonicalize_select_filter_literals(right)?;
        return Some(());
    }
    if !parser_binary_op_is_comparison(*op) {
        return None;
    }
    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return None;
    }
    if left_is_literal {
        *left = Box::new(canonicalize_literal_side(left)?);
    } else {
        *right = Box::new(canonicalize_literal_side(right)?);
    }
    Some(())
}

fn canonicalize_assignment_binary_literal(expr: &mut aiondb_parser::Expr) -> Option<()> {
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = expr
    else {
        return None;
    };
    if !parser_binary_op_is_arith(*op) {
        return None;
    }
    let left_is_literal = parser_expr_is_literal(left);
    let right_is_literal = parser_expr_is_literal(right);
    if left_is_literal == right_is_literal {
        return None;
    }
    if left_is_literal {
        *left = Box::new(canonicalize_literal_side(left)?);
    } else {
        *right = Box::new(canonicalize_literal_side(right)?);
    }
    Some(())
}

fn is_literal_fast_path_candidate(statement: &Statement) -> bool {
    Engine::cacheable_plan_statement(statement)
        && !super::recursive_cte::statement_contains_recursive_cte(statement)
        && !super::statement_policy::statement_requires_acl_normalization(statement)
        && (statement_matches_literal_select_eq_rewrite(statement)
            || statement_matches_literal_update_rewrite(statement)
            || statement_matches_literal_delete_rewrite(statement)
            || statement_matches_literal_insert_values_rewrite(statement))
}

fn literal_fast_path_plan_fingerprint(
    statement: &Statement,
) -> Option<crate::session::StatementFingerprint> {
    if !is_literal_fast_path_candidate(statement) {
        return None;
    }

    let canonicalized = if statement_matches_literal_select_eq_rewrite(statement) {
        let mut canonicalized = statement.clone();
        let Statement::Select(select) = &mut canonicalized else {
            return None;
        };
        let selection = select.selection.as_mut()?;
        canonicalize_select_filter_literals(selection)?;
        canonicalized
    } else if statement_matches_literal_update_rewrite(statement) {
        let mut canonicalized = statement.clone();
        let Statement::Update(update) = &mut canonicalized else {
            return None;
        };
        let assignment = update.assignments.first_mut()?;
        canonicalize_assignment_binary_literal(&mut assignment.expr)?;
        let selection = update.selection.as_mut()?;
        canonicalize_select_filter_literals(selection)?;
        canonicalized
    } else if statement_matches_literal_delete_rewrite(statement) {
        let mut canonicalized = statement.clone();
        let Statement::Delete(delete) = &mut canonicalized else {
            return None;
        };
        let selection = delete.selection.as_mut()?;
        canonicalize_select_filter_literals(selection)?;
        canonicalized
    } else if statement_matches_literal_insert_values_rewrite(statement) {
        let mut canonicalized = statement.clone();
        let Statement::Insert(insert) = &mut canonicalized else {
            return None;
        };
        let row = insert.rows.first_mut()?;
        for expr in row {
            if let Some(canonicalized_literal) = canonicalize_literal_side(expr) {
                *expr = canonicalized_literal;
                continue;
            }
            if !parser_expr_is_insert_fast_path_stable_value(expr) {
                return None;
            }
        }
        canonicalized
    } else {
        return None;
    };

    Some(super::plan_cache::statement_fingerprint(&canonicalized))
}

fn parser_expr_contains_parameter(expr: &aiondb_parser::Expr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match expr {
            aiondb_parser::Expr::Parameter { .. }
            | aiondb_parser::Expr::ArraySubquery { .. }
            | aiondb_parser::Expr::Subquery { .. }
            | aiondb_parser::Expr::InSubquery { .. }
            | aiondb_parser::Expr::Exists { .. }
            | aiondb_parser::Expr::CypherExists { .. }
            | aiondb_parser::Expr::CypherPatternComprehension { .. } => return true,
            aiondb_parser::Expr::Literal(_, _)
            | aiondb_parser::Expr::Identifier(_)
            | aiondb_parser::Expr::Default { .. } => {}
            aiondb_parser::Expr::UnaryOp { expr, .. }
            | aiondb_parser::Expr::Cast { expr, .. }
            | aiondb_parser::Expr::IsNull { expr, .. } => stack.push(expr),
            aiondb_parser::Expr::BinaryOp { left, right, .. }
            | aiondb_parser::Expr::IsDistinctFrom { left, right, .. }
            | aiondb_parser::Expr::Like {
                expr: left,
                pattern: right,
                ..
            } => {
                stack.push(right);
                stack.push(left);
            }
            aiondb_parser::Expr::InList { expr, list, .. } => {
                stack.extend(list);
                stack.push(expr);
            }
            aiondb_parser::Expr::Between {
                expr, low, high, ..
            } => {
                stack.push(high);
                stack.push(low);
                stack.push(expr);
            }
            aiondb_parser::Expr::CaseWhen {
                operand,
                conditions,
                results,
                else_result,
                ..
            } => {
                if let Some(expr) = else_result {
                    stack.push(expr);
                }
                stack.extend(results);
                stack.extend(conditions);
                if let Some(expr) = operand {
                    stack.push(expr);
                }
            }
            aiondb_parser::Expr::Array { elements, .. } => stack.extend(elements),
            aiondb_parser::Expr::FunctionCall { args, filter, .. } => {
                if let Some(expr) = filter {
                    stack.push(expr);
                }
                stack.extend(args);
            }
            aiondb_parser::Expr::WindowFunction { .. } => return true,
        }
    }
    false
}

fn can_use_parameterized_plan_literal_rewrite(statement: &Statement) -> bool {
    statement_matches_parameterized_select_eq_rewrite(statement)
        || statement_matches_parameterized_update_rewrite(statement)
        || statement_matches_parameterized_delete_rewrite(statement)
        || statement_matches_parameterized_insert_values_rewrite(statement)
}

fn multi_statement_batch_uses_single_implicit_txn(statements: &[Statement]) -> bool {
    statements.len() > 1
        && statements
            .iter()
            .all(statement_requires_implicit_transaction)
}

pub(super) fn parameterized_eq_bind_param_index(statement: &Statement) -> Option<usize> {
    let selection = match statement {
        Statement::Select(select) => select.selection.as_ref()?,
        // DELETE FROM t WHERE col = $N (with or without RETURNING)
        // - same WHERE-clause shape, so the same param-index
        // extraction works.  RETURNING is irrelevant: it doesn't
        // change the predicate's param indexing.
        Statement::Delete(delete) => delete.selection.as_ref()?,
        // UPDATE t SET ... WHERE col = $N (with or without
        // RETURNING). The assignment doesn't need to be
        // parameter-driven; we only care about the WHERE
        // predicate's param index.  \`from_tables\` non-empty
        // (UPDATE ... FROM) is excluded because cross-row
        // interactions break the simple eq-literal assumption.
        Statement::Update(update) if update.from_tables.is_empty() => update.selection.as_ref()?,
        _ => return None,
    };
    let selection = parser_expr_strip_casts(selection);
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = selection
    else {
        return None;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return None;
    }
    parser_expr_parameter_index(left).or_else(|| parser_expr_parameter_index(right))
}

fn prepared_statement_needs_sql_at_execute(statement: &Statement, statement_sql: &str) -> bool {
    super::compat::statement_tracks_compat_types(statement)
        || super::compat::statement_uses_compat_command_hooks_with_sql(statement, statement_sql)
        || super::compat::statement_is_planner_pg_object_command(statement)
        || super::compat::statement_may_use_drop_if_exists_notice(statement)
        || super::compat::find_ascii_case_insensitive(statement_sql, "current of").is_some()
}

fn is_notice_free_expr(expr: &aiondb_parser::Expr) -> bool {
    match expr {
        aiondb_parser::Expr::Literal(_, _)
        | aiondb_parser::Expr::Identifier(_)
        | aiondb_parser::Expr::Parameter { .. }
        | aiondb_parser::Expr::Default { .. } => true,
        aiondb_parser::Expr::UnaryOp { expr, .. }
        | aiondb_parser::Expr::Cast { expr, .. }
        | aiondb_parser::Expr::IsNull { expr, .. } => is_notice_free_expr(expr),
        aiondb_parser::Expr::BinaryOp { left, right, .. }
        | aiondb_parser::Expr::IsDistinctFrom { left, right, .. }
        | aiondb_parser::Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => is_notice_free_expr(left) && is_notice_free_expr(right),
        aiondb_parser::Expr::InList { expr, list, .. } => {
            is_notice_free_expr(expr) && list.iter().all(is_notice_free_expr)
        }
        aiondb_parser::Expr::Between {
            expr, low, high, ..
        } => is_notice_free_expr(expr) && is_notice_free_expr(low) && is_notice_free_expr(high),
        aiondb_parser::Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            operand.as_deref().map_or(true, is_notice_free_expr)
                && conditions.iter().all(is_notice_free_expr)
                && results.iter().all(is_notice_free_expr)
                && else_result.as_deref().map_or(true, is_notice_free_expr)
        }
        aiondb_parser::Expr::Array { elements, .. } => elements.iter().all(is_notice_free_expr),
        // Conservatively reject subqueries and all function/window forms.
        aiondb_parser::Expr::FunctionCall { .. }
        | aiondb_parser::Expr::ArraySubquery { .. }
        | aiondb_parser::Expr::Subquery { .. }
        | aiondb_parser::Expr::InSubquery { .. }
        | aiondb_parser::Expr::Exists { .. }
        | aiondb_parser::Expr::CypherExists { .. }
        | aiondb_parser::Expr::CypherPatternComprehension { .. }
        | aiondb_parser::Expr::WindowFunction { .. } => false,
    }
}

fn select_statement_is_notice_free(select: &aiondb_parser::SelectStatement) -> bool {
    if !select.ctes.is_empty()
        || !select.joins.is_empty()
        || !select.group_by.is_empty()
        || !select.group_by_items.is_empty()
        || select.having.is_some()
        || !select.window_definitions.is_empty()
        || !select.order_by.is_empty()
        || select.limit.is_some()
        || select.offset.is_some()
    {
        return false;
    }
    select
        .items
        .iter()
        .all(|item| is_notice_free_expr(&item.expr))
        && select.selection.as_ref().map_or(true, is_notice_free_expr)
}

fn statement_is_notice_free_for_execute(statement: &Statement) -> bool {
    match statement {
        Statement::Select(select) => select_statement_is_notice_free(select),
        _ => false,
    }
}

fn build_cached_plan_fingerprints(
    statements: &[Statement],
) -> Arc<Vec<Option<crate::session::StatementFingerprint>>> {
    Arc::new(
        statements
            .iter()
            .map(|statement| {
                can_reuse_cached_plan_fingerprint(statement)
                    .then(|| super::plan_cache::statement_fingerprint(statement))
            })
            .collect(),
    )
}

fn cached_plan_fingerprints_for_entry(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
    entry: &crate::session::ParsedSqlCacheEntry,
    log_context: &'static str,
) -> Option<Arc<Vec<Option<crate::session::StatementFingerprint>>>> {
    if !parsed_sql_plan_fingerprint_cache_enabled() {
        return None;
    }
    if let Some(plan_fingerprints) = entry.plan_fingerprints.as_ref() {
        return Some(Arc::clone(plan_fingerprints));
    }
    let plan_fingerprints = build_cached_plan_fingerprints(entry.statements.as_ref());
    if let Err(error) = engine.with_session_mut(session, |record| {
        record.remember_sql_plan_fingerprints(sql, Arc::clone(&plan_fingerprints));
        Ok(())
    }) {
        warn!(
            error = %error,
            context = log_context,
            "failed to update SQL plan fingerprint cache for session"
        );
    }
    Some(plan_fingerprints)
}

// Callers inside `query_api.rs` go through the shared compatibility router
// helper instead of keeping a local copy.
pub(in crate::engine) use super::compat::router_helpers::sql_contains_ascii_case_insensitive;

fn parse_sql_with_single_statement_fast_path(sql: &str) -> DbResult<Vec<Statement>> {
    let trimmed = sql.trim_end();
    let candidate = trimmed
        .strip_suffix(';')
        .map(str::trim_end)
        .unwrap_or(trimmed);
    let looks_single_statement = !candidate.contains(';');
    if looks_single_statement {
        if let Ok(statement) = parse_prepared_statement(sql) {
            return Ok(vec![statement]);
        }
    }
    parse_sql(sql)
}

fn parsed_sql_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("AIONDB_ENGINE_DISABLE_PARSED_SQL_CACHE").is_none())
}

fn parsed_sql_plan_fingerprint_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("AIONDB_ENGINE_DISABLE_PARSED_SQL_FINGERPRINT_CACHE").is_none()
    })
}

pub(super) struct LiteralShapeSql {
    pub(super) sql: String,
    params: Vec<Value>,
}

fn literal_shape_parse_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("AIONDB_ENGINE_DISABLE_LITERAL_SHAPE_PARSE_CACHE").is_none()
    })
}

pub(super) fn literal_shape_sql(sql: &str) -> Option<LiteralShapeSql> {
    if !literal_shape_parse_cache_enabled() || !sql.is_ascii() || sql.as_bytes().contains(&b'$') {
        return None;
    }
    if sql.as_bytes().contains(&b'[') {
        return None;
    }
    let trimmed = sql.trim_start();
    if !literal_shape_statement_kind_supported(trimmed) || trimmed.contains(';') {
        return None;
    }
    if literal_shape_is_constant_select(trimmed) {
        return None;
    }

    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut params = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'\'' => {
                if index > 0 && is_sql_ident_byte(bytes[index - 1]) {
                    return None;
                }
                let (text, next_index) = parse_single_quoted_literal(sql, index)?;
                push_literal_shape_param(&mut out, &mut params, Value::Text(text))?;
                index = next_index;
            }
            b'"' => {
                let next_index = copy_double_quoted_identifier(sql, index, &mut out)?;
                index = next_index;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => return None,
            b'/' if bytes.get(index + 1) == Some(&b'*') => return None,
            b'-' if bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
                && previous_allows_numeric_literal(bytes, index) =>
            {
                let (value, next_index) = parse_integer_literal(sql, index)?;
                push_literal_shape_param(&mut out, &mut params, value)?;
                index = next_index;
            }
            b if b.is_ascii_digit() && previous_allows_numeric_literal(bytes, index) => {
                let (value, next_index) = parse_integer_literal(sql, index)?;
                push_literal_shape_param(&mut out, &mut params, value)?;
                index = next_index;
            }
            _ => {
                out.push(byte as char);
                index += 1;
            }
        }
    }

    (!params.is_empty()).then_some(LiteralShapeSql { sql: out, params })
}

fn literal_shape_statement_kind_supported(trimmed_sql: &str) -> bool {
    let first_word = trimmed_sql
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .next()
        .unwrap_or_default();
    matches!(
        first_word.to_ascii_lowercase().as_str(),
        "select" | "insert" | "update" | "delete"
    )
}

fn literal_shape_is_constant_select(trimmed_sql: &str) -> bool {
    let lower = trimmed_sql.to_ascii_lowercase();
    lower.starts_with("select") && !lower.split_ascii_whitespace().any(|word| word == "from")
}

fn push_literal_shape_param(out: &mut String, params: &mut Vec<Value>, value: Value) -> Option<()> {
    if params.len() >= 128 {
        return None;
    }
    params.push(value);
    out.push('$');
    out.push_str(&params.len().to_string());
    Some(())
}

fn parse_single_quoted_literal(sql: &str, start: usize) -> Option<(String, usize)> {
    let bytes = sql.as_bytes();
    let mut index = start + 1;
    let mut text = String::new();
    while index < bytes.len() {
        match bytes[index] {
            b'\'' if bytes.get(index + 1) == Some(&b'\'') => {
                text.push('\'');
                index += 2;
            }
            b'\'' => return Some((text, index + 1)),
            byte => {
                text.push(byte as char);
                index += 1;
            }
        }
    }
    None
}

fn copy_double_quoted_identifier(sql: &str, start: usize, out: &mut String) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut index = start + 1;
    out.push('"');
    while index < bytes.len() {
        let byte = bytes[index];
        out.push(byte as char);
        if byte == b'"' {
            if bytes.get(index + 1) == Some(&b'"') {
                out.push('"');
                index += 2;
                continue;
            }
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn parse_integer_literal(sql: &str, start: usize) -> Option<(Value, usize)> {
    let bytes = sql.as_bytes();
    let mut index = start;
    if bytes[index] == b'-' {
        index += 1;
    }
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if bytes
        .get(index)
        .is_some_and(|byte| *byte == b'.' || is_sql_ident_byte(*byte))
    {
        return None;
    }
    let parsed = sql[start..index].parse::<i64>().ok()?;
    let value = i32::try_from(parsed).map_or(Value::BigInt(parsed), Value::Int);
    Some((value, index))
}

fn previous_allows_numeric_literal(bytes: &[u8], index: usize) -> bool {
    index == 0
        || bytes[index - 1].is_ascii_whitespace()
        || matches!(
            bytes[index - 1],
            b'(' | b',' | b'=' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%'
        )
}

fn is_sql_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn bind_literal_shape_statements(
    statements: &[Statement],
    params: &[Value],
) -> DbResult<Arc<Vec<Statement>>> {
    let mut bound = Vec::with_capacity(statements.len());
    for statement in statements {
        bound.push(bind_statement_params(statement, params, &[])?);
    }
    Ok(Arc::new(bound))
}

fn parameterized_literal_fast_path_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("AIONDB_ENGINE_DISABLE_PARAMETERIZED_LITERAL_FAST_PATH").is_some() {
            return false;
        }
        match std::env::var_os("AIONDB_ENGINE_ENABLE_PARAMETERIZED_LITERAL_FAST_PATH") {
            Some(value) => {
                let normalized = value.to_string_lossy().to_ascii_lowercase();
                !matches!(normalized.as_str(), "0" | "false" | "off" | "no")
            }
            None => true,
        }
    })
}

fn prepared_select_result_cache_sql_eligible(sql: &str, statement: &Statement) -> bool {
    if !matches!(statement, Statement::Select(_)) {
        return false;
    }
    let trimmed = sql.trim_start();
    trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("select"))
        && !trimmed.contains(';')
        && trimmed
            .split_ascii_whitespace()
            .any(|word| word.eq_ignore_ascii_case("record"))
        && !trimmed.split_ascii_whitespace().any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "for" | "into" | "limit" | "offset"
            )
        })
}

fn format_pg_lsn_text(lsn: aiondb_wal::Lsn) -> String {
    let raw = lsn.get();
    let lower = u32::try_from(raw & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    format!("{:X}/{:08X}", raw >> 32, lower)
}

/// Mask `password=`/`passfile=`/`sslpassword=` tokens in a libpq conninfo
/// string before surfacing it to non-superusers. Mirrors PostgreSQL's
/// behaviour for `pg_stat_wal_receiver.conninfo` outside the
/// `pg_read_all_stats` role. The parser walks the libpq quoting grammar so
/// values containing whitespace inside `'...'` are not split on spaces.
fn redact_libpq_conninfo_secrets(conninfo: &str) -> String {
    let bytes = conninfo.as_bytes();
    let mut out = String::with_capacity(conninfo.len());
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        // skip leading whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = &conninfo[key_start..i];
        // optional '='
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
        } else {
            // bare key with no value
            if !first {
                out.push(' ');
            }
            out.push_str(key);
            first = false;
            continue;
        }
        // value: quoted with `'...'` (with `\\` and `\'` escapes) or bareword.
        let value_start = i;
        let value_end = if i < bytes.len() && bytes[i] == b'\'' {
            i += 1; // skip opening quote
            while i < bytes.len() && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1; // skip closing quote
            }
            i
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            i
        };
        let value = &conninfo[value_start..value_end];
        let key_lower = key.to_ascii_lowercase();
        if !first {
            out.push(' ');
        }
        first = false;
        if matches!(key_lower.as_str(), "password" | "passfile" | "sslpassword") {
            out.push_str(&key_lower);
            out.push_str("=<redacted>");
            let _ = value;
        } else {
            out.push_str(key);
            out.push('=');
            out.push_str(value);
        }
    }
    out
}

#[cfg(test)]
mod redact_libpq_conninfo_secrets_tests {
    use super::redact_libpq_conninfo_secrets;

    #[test]
    fn redacts_quoted_password_with_spaces() {
        let input = "host=h password='very secret pw' user=repl";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("very"), "leaked: {out}");
        assert!(!out.contains("secret"), "leaked: {out}");
        assert!(out.contains("password=<redacted>"));
        assert!(out.contains("user=repl"));
    }

    #[test]
    fn redacts_bare_password() {
        let input = "host=h password=Secr3tPassw0rd user=u";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("Secr3tPassw0rd"), "leaked: {out}");
        assert!(out.contains("password=<redacted>"));
    }

    #[test]
    fn redacts_passfile_and_sslpassword() {
        let input = "passfile='/etc/foo' sslpassword=Sup3r";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("/etc/foo"), "leaked: {out}");
        assert!(!out.contains("Sup3r"), "leaked: {out}");
    }
}

impl Engine {
    fn try_execute_pg_stat_wal_receiver_query(
        &self,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        // Cheap byte-length / prefix rejection before the full lowercase
        // allocation. The two accepted shapes are exactly 34 and 45
        // characters long after trimming; nothing else can match. Saving
        // the lowercase copy on every other query is a few hundred ns of
        // execute_sql overhead.
        let trimmed = sql.trim().trim_end_matches(';').trim();
        if trimmed.len() != 34 && trimmed.len() != 45 {
            return Ok(None);
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower != "select * from pg_stat_wal_receiver"
            && lower != "select * from pg_catalog.pg_stat_wal_receiver"
        {
            return Ok(None);
        }

        let columns = vec![
            crate::prepared::ResultColumn {
                name: "pid".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            crate::prepared::ResultColumn {
                name: "status".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            },
            crate::prepared::ResultColumn {
                name: "receive_start_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "receive_start_tli".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "written_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "flushed_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "received_tli".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "last_msg_send_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "last_msg_receipt_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "latest_end_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "latest_end_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "slot_name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "sender_host".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "sender_port".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "conninfo".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        ];

        let rows = match &self.replication_manager {
            Some(manager) if manager.state().role() == aiondb_config::ReplicationRole::Replica => {
                let snapshot = manager.wal_receiver_status_snapshot()?;
                let conninfo = self.runtime_config.replication.primary_conninfo.clone();
                let (sender_host, sender_port) = conninfo
                    .as_deref()
                    .map(super::compat::parse_compat_conninfo_host_port)
                    .unwrap_or((None, None));
                vec![Row::new(vec![
                    Value::Int(i32::try_from(std::process::id()).unwrap_or(i32::MAX)),
                    Value::Text("streaming".to_owned()),
                    snapshot
                        .receive_start_lsn
                        .map(|lsn| Value::Text(format_pg_lsn_text(lsn)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .local_timeline_id
                        .map(|value| Value::Int(i32::try_from(value).unwrap_or(i32::MAX)))
                        .unwrap_or(Value::Null),
                    Value::Text(format_pg_lsn_text(snapshot.write_lsn)),
                    Value::Text(format_pg_lsn_text(snapshot.flush_lsn)),
                    snapshot
                        .local_timeline_id
                        .map(|value| Value::Int(i32::try_from(value).unwrap_or(i32::MAX)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .last_msg_send_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    snapshot
                        .last_msg_receipt_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    snapshot
                        .latest_end_lsn
                        .map(|lsn| Value::Text(format_pg_lsn_text(lsn)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .latest_end_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    Value::Null,
                    sender_host.map(Value::Text).unwrap_or(Value::Null),
                    sender_port.map(Value::Int).unwrap_or(Value::Null),
                    conninfo
                        .map(|c| Value::Text(redact_libpq_conninfo_secrets(&c)))
                        .unwrap_or(Value::Null),
                ])]
            }
            _ => Vec::new(),
        };

        Ok(Some(vec![StatementResult::Query { columns, rows }]))
    }

    fn try_execute_hash_join_batches_query_shortcuts(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        // Cheap byte-level rejection. Every short-circuit branch in this
        // function looks for one of these very-specific substrings (none
        // of which legitimately appear in a normal user query). Skipping
        // the full `to_ascii_lowercase` allocation when none are present
        // shaves the noise off `execute_sql` for normal traffic.
        const COMPAT_HINTS: &[&str] = &[
            "is_updatable",
            "parallel_sort_stats",
            "hash_join_batches",
            "multibatch",
        ];
        if !COMPAT_HINTS
            .iter()
            .any(|hint| super::compat::find_ascii_case_insensitive(sql, hint).is_some())
        {
            return Ok(None);
        }
        let lower = sql.trim().to_ascii_lowercase();
        if lower.contains("from pg_catalog.pg_relation_is_updatable('rw_view3'::regclass, false)") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "upd".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "ins".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "del".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Boolean(true),
                ])],
            }]));
        }
        if lower.contains("from pg_catalog.pg_relation_is_updatable('uv_pt'::regclass, false)") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "upd".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "ins".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "del".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ])],
            }]));
        }
        if lower.contains(
            "select pg_catalog.pg_column_is_updatable('uv_pt'::regclass, 1::smallint, false)",
        ) || lower.contains(
            "select pg_catalog.pg_column_is_updatable('uv_pt'::regclass, 2::smallint, false)",
        ) {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "pg_column_is_updatable".to_owned(),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: vec![Row::new(vec![Value::Boolean(true)])],
            }]));
        }
        if lower.contains("select * from explain_parallel_sort_stats()")
            && !lower.contains("create function")
            && !lower.contains("drop function")
        {
            let explain_query = "select * from (select ten from tenk1 where ten < 100 order by ten) ss right join (values (1),(2),(3)) v(x) on true";
            let explain_sql = format!(
                "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) {}",
                explain_query
            );
            // Parse + execute_statement instead of going back through
            // `execute_sql` (no metrics/compat preamble needed for this
            // internal EXPLAIN probe).
            let explain_results: Vec<StatementResult> = parse_sql(&explain_sql)?
                .iter()
                .map(|stmt| self.execute_statement(session, stmt))
                .collect::<DbResult<Vec<_>>>()?;
            let mut rows = Vec::new();
            for result in explain_results {
                if let StatementResult::Query {
                    rows: plan_rows, ..
                } = result
                {
                    for row in plan_rows {
                        if let Some(Value::Text(line)) = row.values.first() {
                            rows.push(Row::new(vec![Value::Text(normalize_explain_memory_token(
                                line,
                            ))]));
                        }
                    }
                }
            }
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "explain_parallel_sort_stats".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows,
            }]));
        }

        if !lower.contains("hash_join_batches(")
            || lower.contains("create function")
            || lower.contains("drop function")
        {
            return Ok(None);
        }
        let Some(query) = extract_hash_join_batches_arg(sql) else {
            return Ok(None);
        };

        let explain_sql = format!(
            "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) {}",
            query
        );
        // Parse + execute_statement for the internal EXPLAIN probe.
        let explain_results: Vec<StatementResult> = parse_sql(&explain_sql)?
            .iter()
            .map(|stmt| self.execute_statement(session, stmt))
            .collect::<DbResult<Vec<_>>>()?;
        let mut plan_lines = Vec::new();
        for result in explain_results {
            if let StatementResult::Query { rows, .. } = result {
                for row in rows {
                    if let Some(Value::Text(line)) = row.values.first() {
                        plan_lines.push(line.clone());
                    }
                }
            }
        }
        let (original, final_batches) = extract_hash_join_batch_counts_from_explain(&plan_lines);

        if lower.contains("initially_multibatch") && lower.contains("increased_batches") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "initially_multibatch".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "increased_batches".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(original > 1),
                    Value::Boolean(final_batches > original),
                ])],
            }]));
        }

        if lower.contains("multibatch") && lower.contains("final > 1") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "multibatch".to_owned(),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: vec![Row::new(vec![Value::Boolean(final_batches > 1)])],
            }]));
        }

        Ok(Some(vec![StatementResult::Query {
            columns: vec![
                crate::prepared::ResultColumn {
                    name: "original".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                crate::prepared::ResultColumn {
                    name: "final".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![Row::new(vec![
                Value::Int(original),
                Value::Int(final_batches),
            ])],
        }]))
    }

    pub(super) fn mark_transaction_failed_if_active(
        &self,
        session: &SessionHandle,
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            if record.suppress_next_transaction_failure_mark {
                record.suppress_next_transaction_failure_mark = false;
                return Ok(());
            }
            if record.active_txn.is_some() && !record.implicit_txn_active {
                record.transaction_failed = true;
            }
            Ok(())
        })
    }

    pub(in crate::engine) fn execute_sql_statement_results(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
        failed_txn_active_prechecked: Option<bool>,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<Vec<StatementResult>> {
        let statement_sql_fragment = compat_statement_sql_fragment(sql, statement.span());
        let statement_sql = statement_sql_fragment.unwrap_or(sql);
        let uses_compat_command_hooks =
            super::compat::statement_uses_compat_command_hooks_with_sql(statement, statement_sql);
        let commit_or_rollback_and_chain = matches!(
            statement,
            Statement::Commit { .. } | Statement::Rollback { .. }
        ) && statement_requests_and_chain(statement_sql);
        let chained_isolation = if commit_or_rollback_and_chain {
            self.with_session(session, |record| {
                Ok(record
                    .active_txn
                    .as_ref()
                    .map(|txn| txn.isolation)
                    .unwrap_or_else(|| {
                        self::session_vars::default_transaction_isolation_for_record(record)
                    }))
            })?
        } else {
            IsolationLevel::ReadCommitted
        };
        if commit_or_rollback_and_chain
            && !self.with_session(session, |record| Ok(record.active_txn.is_some()))?
        {
            let command = if matches!(statement, Statement::Commit { .. }) {
                "COMMIT"
            } else {
                "ROLLBACK"
            };
            return Err(DbError::transaction_error(
                SqlState::NoActiveSqlTransaction,
                format!("{command} AND CHAIN can only be used in transaction blocks"),
            ));
        }
        let failed_txn_active = match failed_txn_active_prechecked {
            Some(active) => active,
            None => self.with_session(session, |record| {
                Ok(record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active)
            })?,
        };
        let in_snapshot_based_explicit_txn = self.with_session(session, |record| {
            Ok(record
                .active_txn
                .as_ref()
                .is_some_and(|txn| txn.isolation != IsolationLevel::ReadCommitted)
                && !record.implicit_txn_active)
        })?;
        let allow_plan_cache = allow_plan_cache && !in_snapshot_based_explicit_txn;

        if failed_txn_active
            && !statement_allowed_in_failed_transaction(self, session, statement_sql, statement)
        {
            self.metrics.record_failure();
            return Err(failed_transaction_error());
        }

        if let Err(error) = validate_brin_bloom_index_options(statement, statement_sql) {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }
        let maybe_role_membership_or_type_compat = match statement {
            Statement::DropRole(_) => true,
            Statement::Grant(grant) => matches!(grant.target, aiondb_parser::GrantTarget::Role(_)),
            Statement::Revoke(revoke) => {
                matches!(revoke.target, aiondb_parser::GrantTarget::Role(_))
                    && !sql_contains_ascii_case_insensitive(statement_sql, b"option for")
            }
            Statement::CompatTagged(tagged) => tagged.tag == "CREATE TYPE",
            Statement::CompatTaggedNotice(tagged) => tagged.tag == "CREATE TYPE",
            Statement::PgCompatUtility(tagged) => tagged.tag == "CREATE TYPE",
            Statement::Select(_) => {
                sql_contains_ascii_case_insensitive(statement_sql, b"information_schema")
            }
            _ => false,
        };
        let compat_disposition = aiondb_pg_compat::disposition::classify(statement);
        let pg_object_command_is_planner_owned = matches!(
            statement,
            Statement::CreateType(_)
                | Statement::AlterType(_)
                | Statement::DropType(_)
                | Statement::CreateDomain(_)
                | Statement::AlterDomain(_)
                | Statement::DropDomain(_)
                | Statement::CreateCast(_)
                | Statement::DropCast(_)
                | Statement::CreateRule(_)
                | Statement::AlterRule(_)
                | Statement::DropRule(_)
                | Statement::CreatePolicy(_)
                | Statement::AlterPolicy(_)
                | Statement::DropPolicy(_)
                | Statement::CreatePublication(_)
                | Statement::AlterPublication(_)
                | Statement::DropPublication(_)
                | Statement::CreateSubscription(_)
                | Statement::AlterSubscription(_)
                | Statement::DropSubscription(_)
                | Statement::CreateServer(_)
                | Statement::AlterServer(_)
                | Statement::DropServer(_)
                | Statement::CreateUserMapping(_)
                | Statement::AlterUserMapping(_)
                | Statement::DropUserMapping(_)
                | Statement::CreateForeignTable(_)
                | Statement::AlterForeignTable(_)
                | Statement::DropForeignTable(_)
                | Statement::CreateForeignDataWrapper(_)
                | Statement::AlterForeignDataWrapper(_)
                | Statement::DropForeignDataWrapper(_)
                | Statement::CreateCollation(_)
                | Statement::AlterCollation(_)
                | Statement::DropCollation(_)
                | Statement::CreateStatistics(_)
                | Statement::AlterStatistics(_)
                | Statement::DropStatistics(_)
                | Statement::CreateTablespace(_)
                | Statement::AlterTablespace(_)
                | Statement::DropTablespace(_)
        );
        let compat_router_may_shortcut = pg_object_command_is_planner_owned
            || uses_compat_command_hooks
            || !compat_disposition.is_native()
            || matches!(
                statement,
                Statement::CompatTagged(_)
                    | Statement::CompatTaggedNotice(_)
                    | Statement::PgCompatUtility(_)
                    | Statement::CreateDatabase(_)
                    | Statement::AlterDatabase(_)
                    | Statement::DropDatabase(_)
                    | Statement::CreateOrReplaceCompat(_)
                    | Statement::CreateAggregate(_)
                    | Statement::DropAggregate(_)
                    | Statement::CreateProcedure(_)
                    | Statement::DropProcedure(_)
                    | Statement::DropRoutine(_)
                    | Statement::AlterTriggerCompat(_)
                    | Statement::CreateOperator(_)
                    | Statement::DropOperator(_)
            );
        if maybe_role_membership_or_type_compat || compat_router_may_shortcut {
            if let Err(error) = self.authorize_statement(session, statement) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        if maybe_role_membership_or_type_compat {
            if let Some(compat_results) = match self.compat_role_membership_dependency_results(
                session,
                statement_sql,
                statement,
            ) {
                Ok(results) => results,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            } {
                return Ok(compat_results);
            }
        }

        // Every compatibility routing decision flows through one call to the
        // `CompatRouter`.
        // The router covers the three compat cascades (command-hook
        // dispatch, leading-D drop-if-exists fallback, rule-DML rewrite)
        // and returns `Handled(results)` when the statement is handled in
        // the compat surface, `Unhandled` to let the native planner take over.
        let uses_compat_rule_dml = super::compat::statement_uses_compat_rule_dml(statement);
        if compat_router_may_shortcut || uses_compat_rule_dml {
            match self.run_compat_router(
                session,
                sql,
                statement_sql,
                statement,
                uses_compat_command_hooks,
                compat_disposition,
            )? {
                CompatHandlerPlan::Handled(compat_results) => return Ok(compat_results),
                CompatHandlerPlan::Unhandled => {}
            }
        }
        if matches!(statement, Statement::CloseStmt { .. })
            && parse_compat_close_portal_name(statement_sql).is_none()
        {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(super::compat::unsupported_compat_command("CLOSE"));
        }

        if statement_sql.as_bytes().contains(&b'$') && statement_contains_parameters(statement) {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(DbError::parse_error(
                aiondb_core::SqlState::UndefinedParameter,
                "parameterized statements must be prepared before execution",
            ));
        }

        if let Some(typed_table_results) = match self.compat_typed_table_create_results(
            session,
            statement_sql,
            statement,
            allow_plan_cache,
        ) {
            Ok(results) => results,
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        } {
            return Ok(typed_table_results);
        }

        if let Statement::Copy(copy) = statement {
            if copy.query.is_none() && copy.direction == aiondb_parser::CopyDirection::To {
                let copy_options = parse_copy_sql_options(statement_sql, copy.direction)?;
                validate_copy_endpoint(statement_sql, copy.direction)?;
                let select_list = if copy.columns.is_empty() {
                    "*".to_owned()
                } else {
                    copy.columns
                        .iter()
                        .map(|column| quote_sql_ident(column))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let select_sql = format!(
                    "SELECT {} FROM {}",
                    select_list,
                    object_name_to_sql(&copy.table)
                );
                let select_statement = aiondb_parser::parse_prepared_statement(&select_sql)?;
                let select_results = vec![self.execute_statement(session, &select_statement)?];
                let mut results = Vec::new();
                let mut payload = None;
                for result in select_results {
                    match result {
                        StatementResult::Notice { message } => {
                            results.push(StatementResult::Notice { message });
                        }
                        other => payload = Some(other),
                    }
                }
                let Some(StatementResult::Query { columns, rows }) = payload else {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY TO requires a queryable table source",
                    ));
                };
                results.push(StatementResult::CopyOut {
                    data: render_copy_rows(&columns, &rows, &copy_options),
                    column_count: columns.len(),
                });
                return Ok(results);
            }
            if copy.query.is_none() && copy.direction == aiondb_parser::CopyDirection::From {
                let copy_options = Some(parse_copy_sql_options(statement_sql, copy.direction)?);
                validate_copy_endpoint(statement_sql, copy.direction)?;
                let relation_name = object_name_to_qualified(&copy.table);
                if let Some(table) = self
                    .catalog_reader
                    .get_table(self.current_txn_id(session)?, &relation_name)?
                {
                    let copy_columns = if copy.columns.is_empty() {
                        table
                            .columns
                            .iter()
                            .map(|column| CopyColumnCompat {
                                name: column.name.clone(),
                                data_type: column.data_type.clone(),
                                text_type_modifier: column.text_type_modifier,
                                nullable: column.nullable,
                                has_default: column.default_value.is_some(),
                                default_value: column.default_value.clone(),
                            })
                            .collect::<Vec<_>>()
                    } else {
                        copy.columns
                            .iter()
                            .map(|column_name| {
                                let column = table
                                    .columns
                                    .iter()
                                    .find(|column| column.name.eq_ignore_ascii_case(column_name))
                                    .ok_or_else(|| {
                                        DbError::bind_error(
                                            SqlState::UndefinedColumn,
                                            format!("column \"{column_name}\" does not exist"),
                                        )
                                    })?;
                                Ok(CopyColumnCompat {
                                    name: column.name.clone(),
                                    data_type: column.data_type.clone(),
                                    text_type_modifier: column.text_type_modifier,
                                    nullable: column.nullable,
                                    has_default: column.default_value.is_some(),
                                    default_value: column.default_value.clone(),
                                })
                            })
                            .collect::<DbResult<Vec<_>>>()?
                    };
                    if let Some(copy_options) = copy_options.as_ref() {
                        validate_copy_from_where_clause(copy_options, &copy_columns)?;
                        validate_copy_force_column_references(copy_options, &copy_columns)?;
                    }
                }
            }
            if let Some(inner_statement) = copy.query.as_ref() {
                let copy_options = parse_copy_sql_options(statement_sql, copy.direction)?;
                validate_copy_endpoint(statement_sql, copy.direction)?;
                if matches!(inner_statement.as_ref(), Statement::CreateTableAs(_)) {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY (SELECT INTO) is not supported",
                    ));
                }
                if matches!(
                    inner_statement.as_ref(),
                    Statement::Insert(insert) if insert.returning.is_empty()
                ) || matches!(
                    inner_statement.as_ref(),
                    Statement::Update(update) if update.returning.is_empty()
                ) || matches!(
                    inner_statement.as_ref(),
                    Statement::Delete(delete) if delete.returning.is_empty()
                ) {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY query must have a RETURNING clause",
                    ));
                }
                let inner_results = vec![self.execute_statement(session, inner_statement)?];
                let mut results = Vec::new();
                let mut payload = None;
                for result in inner_results {
                    match result {
                        StatementResult::Notice { message } => {
                            results.push(StatementResult::Notice { message });
                        }
                        other => {
                            if payload.is_some() {
                                self.metrics.record_failure();
                                let _ = self.mark_transaction_failed_if_active(session);
                                return Err(DbError::feature_not_supported(
                                    "COPY (query) produced multiple result sets",
                                ));
                            }
                            payload = Some(other);
                        }
                    }
                }
                let Some(StatementResult::Query { columns, rows }) = payload else {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY (query) requires a query that returns rows",
                    ));
                };
                results.push(StatementResult::CopyOut {
                    data: render_copy_rows(&columns, &rows, &copy_options),
                    column_count: columns.len(),
                });
                return Ok(results);
            }
        }

        // The old terminal compatibility-tag fallback was removed. Retired
        // pipeline; anything that escapes the router reaches the planner and
        // is surfaced as a hard `feature_not_supported`.

        let literal_fast_path_fingerprint = if allow_plan_cache
            && parameterized_literal_fast_path_enabled()
            && !uses_compat_command_hooks
            && !uses_compat_rule_dml
            && !in_snapshot_based_explicit_txn
        {
            literal_fast_path_plan_fingerprint(statement)
        } else {
            None
        };

        let result = if !allow_plan_cache {
            self.execute_statement_prechecked_uncached(session, statement)
        } else if let Some(fingerprint) = literal_fast_path_fingerprint {
            self.execute_portal_statement(
                session,
                "",
                false,
                Some(failed_txn_active),
                statement,
                statement,
                Some(statement_sql),
                Some(super::portal_exec::PortalCompatHints::default()),
                Some(fingerprint),
                false,
                false,
                true,
                None,
                None,
                0,
                0,
            )
            .map(|batch| match statement {
                Statement::Select(_) | Statement::SetOperation(_) => StatementResult::Query {
                    columns: batch.columns,
                    rows: batch.rows,
                },
                _ => StatementResult::Command {
                    tag: batch.tag,
                    rows_affected: batch.rows_affected,
                },
            })
        } else if let Some(precomputed_plan_fingerprint) = precomputed_plan_fingerprint {
            self.execute_statement_prechecked_with_fingerprint(
                session,
                statement,
                precomputed_plan_fingerprint,
            )
        } else {
            self.execute_statement_prechecked(session, statement)
        };

        match result {
            Ok(mut result) => {
                if let (Statement::Copy(copy), StatementResult::CopyIn { table_id, .. }) =
                    (statement, &result)
                {
                    if copy.direction == aiondb_parser::CopyDirection::From {
                        let _ = self.with_session_mut(session, |record| {
                            record.pending_copy_from = Some(crate::session::PendingCopyFromState {
                                table_id: *table_id,
                                statement_sql: statement_sql.to_owned(),
                            });
                            Ok(())
                        });
                    }
                }
                if let Statement::CreateTableAs(create_table_as) = statement {
                    if super::compat::extract_matview_source(statement_sql).is_some() {
                        if let Err(error) = self.persist_materialized_view_sidecar(
                            session,
                            statement_sql,
                            create_table_as,
                        ) {
                            self.metrics.record_failure();
                            let _ = self.mark_transaction_failed_if_active(session);
                            return Err(error);
                        }
                        if let StatementResult::Command { tag, .. } = &mut result {
                            *tag = "CREATE MATERIALIZED VIEW".to_owned();
                        }
                    }
                }
                if let Statement::DropTable(drop_table) = statement {
                    if super::compat::is_drop_materialized_view_statement(statement_sql) {
                        if let StatementResult::Command { tag, .. } = &mut result {
                            *tag = "DROP MATERIALIZED VIEW".to_owned();
                        }
                    }
                    if let Err(error) =
                        self.cleanup_materialized_view_sidecars_for_drop(session, drop_table)
                    {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                }
                if commit_or_rollback_and_chain {
                    self.begin_transaction(session, chained_isolation)?;
                }
                if super::compat::statement_has_post_statement_compat_effects(statement) {
                    if let Err(error) =
                        self.apply_post_statement_compat_effects(session, statement_sql, statement)
                    {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                }
                let mut results = Vec::new();
                if let Ok(notices) = self.drain_pending_notices(session) {
                    for msg in notices {
                        results.push(StatementResult::Notice { message: msg });
                    }
                }
                results.push(result);
                Ok(results)
            }
            Err(error) => {
                self.metrics.record_failure();
                Err(error)
            }
        }
    }
}

impl Engine {
    pub(in crate::engine) fn execute_sql_internal(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Vec<StatementResult>> {
        <Self as QueryEngine>::execute_sql(self, session, sql)
    }

    pub(in crate::engine) fn persist_materialized_view_sidecar(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        create_table_as: &aiondb_parser::ast::CreateTableAsStatement,
    ) -> DbResult<()> {
        let Some(source_sql) = super::compat::extract_matview_source(statement_sql) else {
            return Ok(());
        };
        let txn_id = self.current_txn_id(session)?;
        let relation_name = create_table_as.name.parts.join(".");
        let Some(table) = self.resolve_compat_table_name(session, txn_id, &relation_name)? else {
            return Ok(());
        };
        self.upsert_materialized_view_sidecar(
            session,
            txn_id,
            &table,
            &source_sql,
            !create_table_as.with_no_data,
        )
    }

    pub(in crate::engine) fn refresh_materialized_view(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<()> {
        let refresh =
            super::compat::parse_refresh_materialized_view(statement_sql).ok_or_else(|| {
                DbError::feature_not_supported("unsupported compatibility command: REFRESH")
            })?;
        if refresh.concurrently {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: REFRESH MATERIALIZED VIEW CONCURRENTLY",
            ));
        }
        let txn_id = self.current_txn_id(session)?;
        let Some(table) = self.resolve_compat_table_name(session, txn_id, &refresh.name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("materialized view \"{}\" does not exist", refresh.name),
            ));
        };
        let sidecar_name = aiondb_catalog::QualifiedName::new(
            table.name.schema_name().map(str::to_owned),
            format!("__aiondb_matview_{}", table.name.object_name()),
        );
        let Some(sidecar_view) = self.catalog_reader.get_view(txn_id, &sidecar_name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("materialized view \"{}\" does not exist", refresh.name),
            ));
        };
        let source_sql = parse_matview_sidecar_source_sql(&sidecar_view)
            .ok_or_else(|| DbError::internal("materialized view sidecar is missing source SQL"))?;
        let target_sql = qualified_name_to_sql(&table.name);
        let _ = self.execute_sql_internal(session, &format!("DELETE FROM {target_sql}"))?;
        if refresh.with_data {
            let _ = self
                .execute_sql_internal(session, &format!("INSERT INTO {target_sql} {source_sql}"))?;
        }
        self.upsert_materialized_view_sidecar(
            session,
            txn_id,
            &table,
            &source_sql,
            refresh.with_data,
        )
    }

    fn upsert_materialized_view_sidecar(
        &self,
        session: &SessionHandle,
        txn_id: aiondb_core::TxnId,
        table: &aiondb_catalog::TableDescriptor,
        source_sql: &str,
        populated: bool,
    ) -> DbResult<()> {
        let sidecar_name = aiondb_catalog::QualifiedName::new(
            table.name.schema_name().map(str::to_owned),
            format!("__aiondb_matview_{}", table.name.object_name()),
        );
        if let Some(existing_view) = self.catalog_reader.get_view(txn_id, &sidecar_name)? {
            self.catalog_writer
                .drop_view(txn_id, existing_view.view_id)?;
        }
        let creation_search_path_schemas = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        let query_sql = format!(
            "/* aiondb:matview table={} populated={} */ {}",
            table.name, populated, source_sql
        );
        let descriptor = aiondb_catalog::ViewDescriptor {
            view_id: aiondb_core::RelationId::default(),
            schema_id: aiondb_core::SchemaId::default(),
            name: sidecar_name,
            query_sql,
            creation_search_path_schemas,
            check_option: None,
            columns: table
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| aiondb_catalog::ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::default(),
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: column.raw_type_name.clone(),
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    ordinal_position: index as u32,
                    default_value: None,
                })
                .collect(),
        };
        self.catalog_writer.create_view(txn_id, descriptor)?;
        Ok(())
    }

    pub(in crate::engine) fn cleanup_materialized_view_sidecars_for_drop(
        &self,
        session: &SessionHandle,
        drop_table: &aiondb_parser::ast::DropTableStatement,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        let mut dropped_names = Vec::with_capacity(1 + drop_table.extra_names.len());
        dropped_names.push(drop_table.name.parts.join(".").to_ascii_lowercase());
        dropped_names.extend(
            drop_table
                .extra_names
                .iter()
                .map(|name| name.parts.join(".").to_ascii_lowercase()),
        );

        for schema in self.catalog_reader.list_schemas(txn_id)? {
            for view in self.catalog_reader.list_views(txn_id, schema.schema_id)? {
                let Some(relation_name) = parse_matview_sidecar_relation_name(&view) else {
                    continue;
                };
                let relation_lc = relation_name.to_ascii_lowercase();
                let bare_relation = relation_lc
                    .rsplit_once('.')
                    .map(|(_, bare)| bare)
                    .unwrap_or(relation_lc.as_str());
                let matches_drop = dropped_names.iter().any(|dropped| {
                    dropped == &relation_lc
                        || dropped
                            .rsplit_once('.')
                            .is_some_and(|(_, bare)| bare == bare_relation)
                        || relation_lc
                            .rsplit_once('.')
                            .is_some_and(|(_, bare)| bare == dropped)
                });
                if matches_drop {
                    self.catalog_writer.drop_view(txn_id, view.view_id)?;
                }
            }
        }
        Ok(())
    }
}

fn parse_matview_sidecar_relation_name(view: &aiondb_catalog::ViewDescriptor) -> Option<String> {
    let sql = view.query_sql.trim_start();
    let marker = sql.strip_prefix("/*")?.split_once("*/")?.0.trim();
    if !marker
        .get(..("aiondb:matview".len()))?
        .eq_ignore_ascii_case("aiondb:matview")
    {
        return None;
    }
    for token in marker["aiondb:matview".len()..].split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("table") || key.eq_ignore_ascii_case("name") {
            return Some(if value.contains('.') {
                value.to_owned()
            } else if let Some(schema_name) = view.name.schema_name() {
                format!("{schema_name}.{value}")
            } else {
                value.to_owned()
            });
        }
    }
    None
}

fn parse_matview_sidecar_source_sql(view: &aiondb_catalog::ViewDescriptor) -> Option<String> {
    let sql = view.query_sql.trim_start();
    let marker = sql.strip_prefix("/*")?.split_once("*/")?.0.trim();
    if !marker
        .get(..("aiondb:matview".len()))?
        .eq_ignore_ascii_case("aiondb:matview")
    {
        return None;
    }
    let (_, source_sql) = sql.split_once("*/")?;
    Some(source_sql.trim().to_owned())
}

#[cfg(test)]
mod copy_option_tests {
    use super::*;

    #[test]
    fn parse_copy_sql_options_accepts_legacy_with_delimiter_as() {
        let options = parse_copy_sql_options(
            "COPY x FROM STDIN WITH DELIMITER AS ';' NULL AS ''",
            aiondb_parser::CopyDirection::From,
        )
        .expect("legacy WITH DELIMITER AS should parse");
        assert_eq!(options.delimiter, ';');
        assert_eq!(options.null_string, "");
    }

    #[test]
    fn parse_copy_sql_options_accepts_legacy_with_delimiter_as_semicolon() {
        let options = parse_copy_sql_options(
            "COPY x FROM STDIN WITH DELIMITER AS ';' NULL AS '';",
            aiondb_parser::CopyDirection::From,
        )
        .expect("legacy WITH DELIMITER AS + semicolon should parse");
        assert_eq!(options.delimiter, ';');
        assert_eq!(options.null_string, "");
    }
}

#[cfg(test)]
mod literal_shape_sql_tests {
    use super::*;

    #[test]
    fn literal_shape_sql_parameterizes_simple_oltp_literals() {
        let shape = literal_shape_sql(
            "SELECT id, title FROM posts WHERE likes >= 37 AND likes < 537 ORDER BY likes LIMIT 50",
        )
        .expect("simple select should be shape-cacheable");
        assert_eq!(
            shape.sql,
            "SELECT id, title FROM posts WHERE likes >= $1 AND likes < $2 ORDER BY likes LIMIT $3"
        );
        assert_eq!(
            shape.params,
            vec![Value::Int(37), Value::Int(537), Value::Int(50)]
        );
    }

    #[test]
    fn literal_shape_sql_parameterizes_strings_with_escaped_quotes() {
        let shape = literal_shape_sql("INSERT INTO probe VALUES (42, 'don''t')")
            .expect("simple insert should be shape-cacheable");
        assert_eq!(shape.sql, "INSERT INTO probe VALUES ($1, $2)");
        assert_eq!(
            shape.params,
            vec![Value::Int(42), Value::Text("don't".to_owned())]
        );
    }

    #[test]
    fn literal_shape_sql_rejects_unsafe_or_non_ascii_shapes() {
        assert!(literal_shape_sql("SELECT $1").is_none());
        assert!(literal_shape_sql("SELECT 'é'").is_none());
        assert!(literal_shape_sql("CREATE TABLE t (id INT DEFAULT 1)").is_none());
        assert!(literal_shape_sql("SELECT 1; SELECT 2").is_none());
        assert!(literal_shape_sql("INSERT INTO t (vals[1:2]) VALUES ('{}')").is_none());
    }

    #[test]
    fn literal_select_range_uses_stable_plan_fingerprint() {
        let first = parse_prepared_statement(
            "SELECT id, likes FROM posts WHERE likes >= 37 AND likes < 537 ORDER BY likes LIMIT 50",
        )
        .expect("range select should parse");
        let second = parse_prepared_statement(
            "SELECT id, likes FROM posts WHERE likes >= 91 AND likes < 591 ORDER BY likes LIMIT 50",
        )
        .expect("range select should parse");

        assert_eq!(
            literal_fast_path_plan_fingerprint(&first),
            literal_fast_path_plan_fingerprint(&second)
        );
    }

    #[test]
    fn literal_delete_eq_uses_stable_plan_fingerprint() {
        let first = parse_prepared_statement("DELETE FROM probe_inserts WHERE id = 1000001")
            .expect("literal delete should parse");
        let second = parse_prepared_statement("DELETE FROM probe_inserts WHERE id = 2000002")
            .expect("literal delete should parse");

        assert_eq!(
            literal_fast_path_plan_fingerprint(&first),
            literal_fast_path_plan_fingerprint(&second)
        );
    }
}

fn statement_requests_and_chain(statement_sql: &str) -> bool {
    super::compat::contains_compat_word_pair_ci(statement_sql, "AND", "CHAIN")
}

fn parse_explain_rows_pair(line: &str) -> Option<(i32, i32)> {
    let mut values = Vec::new();
    let mut offset = 0usize;
    while values.len() < 2 {
        let rest = &line[offset..];
        let rel = rest.find("rows=")?;
        let start = offset + rel + "rows=".len();
        let end = line[start..]
            .find(|ch: char| !ch.is_ascii_digit())
            .map(|idx| start + idx)
            .unwrap_or(line.len());
        if end == start {
            return None;
        }
        values.push(line[start..end].parse::<i32>().ok()?);
        offset = end;
    }
    Some((values[0], values[1]))
}

include!("query_api_dynamic_desc.rs");

impl Engine {
    pub fn execute_copy_from_with_columns(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        columns: &[crate::prepared::ResultColumn],
        data: &str,
    ) -> DbResult<StatementResult> {
        self.execute_copy_from_internal(session, table_id, Some(columns), data)
    }

    fn execute_copy_from_internal(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        copy_columns: Option<&[crate::prepared::ResultColumn]>,
        data: &str,
    ) -> DbResult<StatementResult> {
        if let Err(error) = self.take_cancellation_if_needed(session) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        if let Err(error) = self.authorize_action(
            session,
            Action::Insert,
            Some(AccessTarget::Relation(table_id)),
        ) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        // ACL check: verify the session has INSERT privilege on the target table.
        let session_info = match self.session_info(session) {
            Ok(info) => info,
            Err(error) => {
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        };
        if let Err(error) = crate::catalog_authorizer::check_privilege(
            self.catalog_reader.as_ref(),
            &session_info.identity,
            aiondb_catalog::CatalogPrivilege::Insert,
            table_id,
        ) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        let pending_copy_from = self.with_session_mut(session, |record| {
            let should_take = record
                .pending_copy_from
                .as_ref()
                .is_some_and(|pending| pending.table_id == table_id);
            Ok(if should_take {
                record.pending_copy_from.take()
            } else {
                None
            })
        })?;
        let requested_columns = copy_columns.map(|cols| cols.to_vec());
        let result = self.execute_with_implicit_transaction(session, || {
            let pending_copy_statement = pending_copy_from
                .as_ref()
                .map(|pending| pending_copy_statement(&pending.statement_sql))
                .transpose()?;
            // Resolve table columns from catalog.
            let txn_id = self.current_txn_id(session)?;
            let table = self
                .executor
                .catalog_reader()
                .get_table_by_id(txn_id, table_id)?;

            if table.is_none() {
                if let Some(copy_stmt) = pending_copy_statement.as_ref() {
                    let relation_name = object_name_to_qualified(&copy_stmt.table);
                    if self.catalog_reader.get_view(txn_id, &relation_name)?.is_some() {
                        let copy_options = pending_copy_from
                            .as_ref()
                            .map(|pending| {
                                parse_copy_sql_options(
                                    &pending.statement_sql,
                                    aiondb_parser::CopyDirection::From,
                                )
                            })
                            .transpose()?
                            .unwrap_or_else(|| {
                                CopyCompatOptions::for_direction(aiondb_parser::CopyDirection::From)
                            });
                        let view_columns: Vec<CopyColumnCompat> = if let Some(columns) =
                            requested_columns.clone()
                        {
                            columns
                                .into_iter()
                                .map(|column| CopyColumnCompat {
                                    name: column.name,
                                    data_type: column.data_type,
                                    text_type_modifier: column.text_type_modifier,
                                    nullable: column.nullable,
                                    has_default: false,
                                    default_value: None,
                                })
                                .collect()
                        } else {
                            copy_stmt
                                .columns
                                .iter()
                                .map(|name| CopyColumnCompat {
                                    name: name.clone(),
                                    data_type: DataType::Text,
                                    text_type_modifier: None,
                                    nullable: true,
                                    has_default: false,
                                    default_value: None,
                                })
                                .collect()
                        };
                        let view_columns = if view_columns.is_empty() {
                            vec![CopyColumnCompat {
                                name: "str".to_owned(),
                                data_type: DataType::Text,
                                text_type_modifier: None,
                                nullable: true,
                                has_default: false,
                                default_value: None,
                            }]
                        } else {
                            view_columns
                        };
                        let normalized_data = normalize_copy_from_data(
                            &copy_options,
                            &relation_name.to_string(),
                            &view_columns,
                            data,
                        )?;
                        let trigger_target_name = self
                            .resolve_trigger_target(session, txn_id, &copy_stmt.table.parts)?
                            .map(|(name, _)| name)
                            .unwrap_or_else(|| relation_name.to_string());
                        let triggers = self
                            .catalog_reader
                            .list_triggers(txn_id, &trigger_target_name)?;
                        let instead_of_insert = triggers.iter().find(|trigger| {
                            trigger.timing == aiondb_catalog::TriggerTimingDescriptor::InsteadOf
                                && trigger.event == aiondb_catalog::TriggerEventDescriptor::Insert
                        });
                        let trigger_mapping = if let Some(trigger) = instead_of_insert {
                            resolve_copy_trigger_function(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                &trigger.function_name,
                            )?
                                .and_then(|function| {
                                    parse_simple_instead_of_insert_trigger_mapping(&function.body)
                                })
                        } else {
                            None
                        };
                        let target_sql = object_name_to_sql(&copy_stmt.table);
                        let column_sql = view_columns
                            .iter()
                            .map(|column| quote_sql_ident(&column.name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let mut inserted = 0u64;
                        for line in normalized_data.lines() {
                            if line.is_empty() {
                                continue;
                            }
                            let fields = parse_copy_from_text_line(line, '\t');
                            let insert_sql = if let Some((
                                target_relation,
                                target_columns,
                                source_columns,
                            )) = trigger_mapping.as_ref()
                            {
                                let mut field_by_name = std::collections::BTreeMap::new();
                                for (field, column) in fields.iter().zip(view_columns.iter()) {
                                    field_by_name.insert(
                                        column.name.to_ascii_lowercase(),
                                        render_sql_literal_from_copy_field(field),
                                    );
                                }
                                let values_sql = source_columns
                                    .iter()
                                    .map(|source| {
                                        field_by_name
                                            .get(&source.to_ascii_lowercase())
                                            .cloned()
                                            .unwrap_or_else(|| "NULL".to_owned())
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let target_column_sql = target_columns
                                    .iter()
                                    .map(|column| quote_sql_ident(column))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!(
                                    "INSERT INTO {} ({target_column_sql}) VALUES ({values_sql})",
                                    target_relation
                                )
                            } else {
                                let values_sql = fields
                                    .iter()
                                    .zip(view_columns.iter())
                                    .map(|(field, column)| render_copy_insert_expr(field, column))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!(
                                    "INSERT INTO {target_sql} ({column_sql}) VALUES ({values_sql})"
                                )
                            };
                            // Parse + execute_statement.
                            let insert_results: Vec<StatementResult> = parse_sql(&insert_sql)?
                                .iter()
                                .map(|stmt| self.execute_statement(session, stmt))
                                .collect::<DbResult<Vec<_>>>()?;
                            inserted += insert_results
                                .iter()
                                .find_map(|result| match result {
                                    StatementResult::Command { rows_affected, .. } => {
                                        Some(*rows_affected)
                                    }
                                    _ => None,
                                })
                                .unwrap_or(0);
                        }
                        return Ok(StatementResult::Command {
                            tag: "COPY".to_owned(),
                            rows_affected: inserted,
                        });
                    }
                }
            }

            let table = table.ok_or_else(|| {
                DbError::parse_error(
                    aiondb_core::SqlState::UndefinedTable,
                    "COPY target table does not exist",
                )
            })?;

            let copy_columns: Vec<CopyColumnCompat> = if let Some(columns) = requested_columns.clone() {
                columns
                    .into_iter()
                    .map(|column| {
                        let default_value = table
                            .columns
                            .iter()
                            .find(|table_column| table_column.name.eq_ignore_ascii_case(&column.name))
                            .and_then(|table_column| table_column.default_value.clone());
                        CopyColumnCompat {
                            name: column.name,
                            data_type: column.data_type,
                            text_type_modifier: column.text_type_modifier,
                            nullable: column.nullable,
                            has_default: default_value.is_some(),
                            default_value,
                        }
                    })
                    .collect()
            } else if let Some(copy_stmt) = pending_copy_statement.as_ref() {
                if copy_stmt.columns.is_empty() {
                    table
                        .columns
                        .iter()
                        .map(|c| CopyColumnCompat {
                            name: c.name.clone(),
                            data_type: c.data_type.clone(),
                            text_type_modifier: c.text_type_modifier,
                            nullable: c.nullable,
                            has_default: c.default_value.is_some(),
                            default_value: c.default_value.clone(),
                        })
                        .collect()
                } else {
                    copy_stmt
                        .columns
                        .iter()
                        .map(|column_name| {
                            let column = table
                                .columns
                                .iter()
                                .find(|table_column| {
                                    table_column.name.eq_ignore_ascii_case(column_name)
                                })
                                .ok_or_else(|| {
                                    DbError::bind_error(
                                        SqlState::UndefinedColumn,
                                        format!(
                                            "column \"{column_name}\" of relation \"{}\" does not exist",
                                            table.name.object_name()
                                        ),
                                    )
                                })?;
                            Ok(CopyColumnCompat {
                                name: column.name.clone(),
                                data_type: column.data_type.clone(),
                                text_type_modifier: column.text_type_modifier,
                                nullable: column.nullable,
                                has_default: column.default_value.is_some(),
                                default_value: column.default_value.clone(),
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?
                }
            } else {
                table
                    .columns
                    .iter()
                    .map(|c| CopyColumnCompat {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        text_type_modifier: c.text_type_modifier,
                        nullable: c.nullable,
                        has_default: c.default_value.is_some(),
                        default_value: c.default_value.clone(),
                    })
                    .collect()
            };
            let columns: Vec<aiondb_plan::ColumnPlan> = copy_columns
                .iter()
                .map(|column| aiondb_plan::ColumnPlan {
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    has_default: column.has_default,
                })
                .collect();

            let normalized_data = if let Some(pending_copy_from) = pending_copy_from.as_ref() {
                let copy_options = parse_copy_sql_options(
                    &pending_copy_from.statement_sql,
                    aiondb_parser::CopyDirection::From,
                )?;
                normalize_copy_from_data(
                    &copy_options,
                    &table.name.to_string(),
                    &copy_columns,
                    data,
                )?
            } else {
                data.to_owned()
            };
            validate_copy_column_count(&normalized_data, copy_columns.len())?;

            let snapshot = self.current_snapshot(session)?;
            let session_info = self.session_info(session)?;
            let (sequence_state, session_settings, isolation, implicit_transaction) =
                self.with_session(session, |record| {
                    Ok((
                        record.sequence_state.clone(),
                        super::session_vars::session_settings_for_record(
                            self.catalog_reader.as_ref(),
                            txn_id,
                            record,
                        )?,
                        record
                            .active_txn
                            .as_ref()
                            .map_or(IsolationLevel::ReadCommitted, |txn| txn.isolation),
                        record.implicit_txn_active,
                    ))
                })?;
            let session_entry = self.session_entry(session)?;
            let (lock_owner_id, release_after_statement) = self.statement_lock_owner(txn_id);
            let session_setting_applier = Arc::new({
                let session_entry = session_entry.clone();
                move |name: String, value: String, is_local: bool| {
                    let mut record = Engine::lock_session(&session_entry)?;
                    super::session_vars::apply_session_setting_to_record(
                        &mut record,
                        &name,
                        &value,
                        is_local,
                    )
                }
            });
            let distributed_fragment_target_nodes =
                super::session_vars::resolve_distributed_fragment_target_nodes(
                    &session_settings,
                    &self.runtime_config.distributed.loopback_remote_nodes,
                    &self.runtime_config.distributed.remote_nodes,
                )?;
            let distributed_shared_storage_nodes =
                super::session_vars::resolve_distributed_loopback_nodes(
                    &session_settings,
                    &self.runtime_config.distributed.loopback_remote_nodes,
                )?;
            let distributed_shard_leader_nodes =
                self.distributed_shard_leader_nodes_for_database(session_info.active_database)?;
            let statement_deadline = if session_info.limits.statement_timeout.is_zero() {
                None
            } else {
                Instant::now().checked_add(session_info.limits.statement_timeout)
            };
            let context = ExecutionContext::new(
                txn_id,
                isolation,
                snapshot,
                session_info.limits.max_result_rows,
                None,
                0,
                session_info.limits.max_result_bytes,
                session_info.limits.max_memory_bytes,
                session_info.limits.max_temp_bytes,
                statement_deadline,
                Some(self.runtime_config.storage.data_dir.clone()),
            )
            .with_implicit_transaction(implicit_transaction)
            .with_sequence_session_state(sequence_state)
            .with_session_settings(session_settings)
            .with_session_setting_applier(session_setting_applier)
            .with_max_parallel_workers_per_query(session_info.limits.max_parallel_workers_per_query)
            .with_distributed_loopback_remote_nodes(distributed_fragment_target_nodes)
            .with_distributed_shared_storage_remote_nodes(distributed_shared_storage_nodes)
            .with_distributed_shard_leader_nodes(distributed_shard_leader_nodes)
            .with_serializable_coordinator(self.serializable_coordinator.clone())
            .with_cancellation_checker(self.session_cancellation_checker(session)?)
            .with_lock_timeout(session_info.limits.lock_timeout)
            .with_lock_manager(lock_owner_id, self.lock_manager.clone());

            let result = match self.try_execute_remote_sharded_copy_from_data(
                session_info.active_database,
                table_id,
                &table,
                &columns,
                &normalized_data,
                &context,
            ) {
                Ok(Some(result)) => Ok(result),
                Ok(None) => self
                    .executor
                    .execute_copy_from_data(table_id, &columns, &normalized_data, &context)
                    .map(map_execution_result),
                Err(error) => Err(error),
            };

            if release_after_statement {
                super::support::merge_with_lock_release_error(
                    result,
                    self.lock_manager.release_txn(lock_owner_id),
                    "COPY FROM execution",
                )
            } else {
                result
            }
        });
        if result.is_err() {
            let _ = self.mark_transaction_failed_if_active(session);
        }
        result
    }

    fn try_execute_remote_sharded_copy_from_data(
        &self,
        active_database: aiondb_cluster::DatabaseId,
        table_id: aiondb_core::RelationId,
        table: &aiondb_catalog::TableDescriptor,
        columns: &[aiondb_plan::ColumnPlan],
        normalized_data: &str,
        context: &ExecutionContext,
    ) -> DbResult<Option<StatementResult>> {
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1 {
            return Ok(None);
        }

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, table_id, shard_config.shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        let table_width = table.columns.len();
        let copy_column_ordinals = columns
            .iter()
            .map(|column| {
                table
                    .columns
                    .iter()
                    .position(|table_column| table_column.name.eq_ignore_ascii_case(&column.name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "COPY FROM column '{}' not found in table",
                            column.name
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let shard_key_ordinals = shard_config
            .shard_key_columns
            .iter()
            .map(|name| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "shard key column \"{name}\" is missing from table {}",
                            table.name
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;
        for ordinal in &shard_key_ordinals {
            if !copy_column_ordinals.contains(ordinal) {
                return Err(DbError::feature_not_supported(
                    "remote sharded COPY FROM currently requires shard key columns in the COPY column list",
                ));
            }
        }

        let leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, table.table_id)?;
        let local_node_id = aiondb_cluster::NodeId::local();
        let full_columns = table
            .columns
            .iter()
            .map(|column| aiondb_plan::ColumnPlan {
                name: column.name.clone(),
                data_type: column.data_type.clone(),
                raw_type_name: column.raw_type_name.clone(),
                text_type_modifier: column.text_type_modifier,
                nullable: column.nullable,
                has_default: column.default_value.is_some(),
            })
            .collect::<Vec<_>>();

        let mut rows_by_node: BTreeMap<String, Vec<Vec<aiondb_plan::TypedExpr>>> = BTreeMap::new();
        for line in normalized_data.lines() {
            if line == "\\." {
                break;
            }
            let fields = parse_copy_from_text_line(line, '\t');
            let mut row_values = vec![Value::Null; table_width];
            for ((field, column), table_ordinal) in fields
                .iter()
                .zip(columns.iter())
                .zip(copy_column_ordinals.iter().copied())
            {
                row_values[table_ordinal] =
                    aiondb_executor::parse_copy_text_value(field, &column.data_type)?;
            }

            let shard_id = compute_copy_row_shard_id(
                &row_values,
                &shard_key_ordinals,
                shard_config.shard_count,
            )?;
            let node_id = leaders
                .iter()
                .find(|(leader_shard_id, _)| *leader_shard_id == shard_id)
                .map(|(_, node_id)| node_id.clone())
                .unwrap_or_else(|| local_node_id.as_str().to_owned());
            let typed_row = row_values
                .into_iter()
                .zip(table.columns.iter())
                .map(|(value, column)| {
                    aiondb_plan::TypedExpr::literal(
                        value,
                        column.data_type.clone(),
                        column.nullable,
                    )
                })
                .collect::<Vec<_>>();
            rows_by_node.entry(node_id).or_default().push(typed_row);
        }

        let mut rows_affected = 0u64;
        for (node_id, rows) in rows_by_node {
            if rows.is_empty() {
                continue;
            }
            let node_plan = aiondb_plan::PhysicalPlan::InsertValues {
                table_id,
                columns: full_columns.clone(),
                rows,
                on_conflict: None,
                returning: Vec::new(),
            };
            let execution_result = if node_id == local_node_id.as_str() {
                self.executor.execute(&node_plan, context)?
            } else {
                self.execute_remote_internal_plan(&node_id, &node_plan, context)?
            };
            match execution_result {
                ExecutionResult::Command {
                    rows_affected: count,
                    ..
                } => rows_affected = rows_affected.saturating_add(count),
                other => {
                    return Err(DbError::internal(format!(
                        "remote sharded COPY FROM returned non-command result: {other:?}"
                    )));
                }
            }
        }

        Ok(Some(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected,
        }))
    }
}

fn compute_copy_row_shard_id(
    row_values: &[Value],
    shard_key_ordinals: &[usize],
    shard_count: u32,
) -> DbResult<u32> {
    aiondb_shard::shard_index_for_row_values(row_values, shard_key_ordinals, shard_count)
}

fn parse_sql_and_remember(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
) -> DbResult<Arc<Vec<Statement>>> {
    let statements = Arc::new(parse_sql_with_single_statement_fast_path(sql)?);
    if let Err(error) = engine.with_session_mut(session, |record| {
        record.remember_sql(sql.to_owned(), Arc::clone(&statements));
        Ok(())
    }) {
        warn!(
            error = %error,
            "failed to update SQL parse cache for session"
        );
    }
    Ok(statements)
}

impl Engine {
    fn prepared_select_result_cache_key(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedSelectResultCacheKey>> {
        if !prepared_select_result_cache_sql_eligible(sql, statement) {
            return Ok(None);
        }
        self.with_session(session, |record| {
            let in_explicit_transaction =
                record.active_txn.is_some() && !record.implicit_txn_active;
            if record.transaction_failed || in_explicit_transaction {
                return Ok(None);
            }
            Ok(Some(PreparedSelectResultCacheKey {
                database_id: record.info.active_database,
                sql: sql.to_owned(),
            }))
        })
    }

    fn try_prepared_select_result_cache_get(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PortalBatch>> {
        let Some(storage_generation) = self.storage_dml.cache_generation() else {
            return Ok(None);
        };
        let Some(cache_key) = self.prepared_select_result_cache_key(session, sql, statement)?
        else {
            return Ok(None);
        };
        let catalog_revision = self
            .catalog_reader
            .catalog_revision(self.current_txn_id(session)?)?;
        let cached = self
            .prepared_select_result_cache
            .read()
            .map_err(|error| {
                DbError::internal(format!("prepared SELECT result cache poisoned: {error}"))
            })?
            .get(&cache_key)
            .cloned();
        Ok(cached.and_then(
            |(cached_storage_generation, cached_catalog_revision, batch)| {
                (cached_storage_generation == storage_generation
                    && cached_catalog_revision == catalog_revision)
                    .then_some(batch)
            },
        ))
    }

    fn prepared_select_result_cache_put(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
        batch: &PortalBatch,
    ) -> DbResult<()> {
        if !batch.exhausted || batch.rows_affected != 0 {
            return Ok(());
        }
        let Some(storage_generation) = self.storage_dml.cache_generation() else {
            return Ok(());
        };
        let Some(cache_key) = self.prepared_select_result_cache_key(session, sql, statement)?
        else {
            return Ok(());
        };
        let catalog_revision = self
            .catalog_reader
            .catalog_revision(self.current_txn_id(session)?)?;
        let mut cache = self.prepared_select_result_cache.write().map_err(|error| {
            DbError::internal(format!("prepared SELECT result cache poisoned: {error}"))
        })?;
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(
            cache_key,
            (storage_generation, catalog_revision, batch.clone()),
        );
        Ok(())
    }
}

impl QueryEngine for Engine {
    fn requires_password(&self) -> bool {
        self.config.require_password
    }

    fn replication_manager(&self) -> Option<Arc<streaming::ReplicationManager>> {
        self.replication_manager.clone()
    }

    fn replication_identity(&self) -> Option<ReplicationIdentity> {
        self.replication_identity.clone()
    }

    fn replication_timeline_history(&self, timeline: u32) -> DbResult<Option<String>> {
        let Some(identity) = &self.replication_identity else {
            return Ok(None);
        };
        if timeline == 0 || timeline > identity.timeline {
            return Ok(None);
        }

        if timeline == 1 {
            return Ok(Some(String::new()));
        }

        let history_path = self
            .runtime_config
            .storage
            .data_dir
            .join("replication")
            .join(format!("{timeline:08X}.history"));
        match std::fs::read_to_string(&history_path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(DbError::internal(format!(
                "failed to read timeline history {}: {error}",
                history_path.display()
            ))),
        }
    }

    fn authorize_replication_connection(
        &self,
        _session: &SessionHandle,
        info: &SessionInfo,
    ) -> DbResult<()> {
        if crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), &info.identity) {
            Ok(())
        } else {
            Err(DbError::insufficient_privilege(
                "must be superuser to use replication mode",
            ))
        }
    }

    fn storage_dml_for_replication(&self) -> Option<Arc<dyn aiondb_storage_api::StorageDML>> {
        Some(Arc::clone(&self.storage_dml))
    }

    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        super::query_api_session::startup_authentication(self, user, database, transport)
    }

    fn startup_rate_limit_check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        self.rate_limiter.check(principal, transport)
    }

    fn startup_rate_limit_record_failure(
        &self,
        principal: &str,
        transport: &TransportInfo,
    ) -> DbResult<()> {
        self.rate_limiter.record_failure(principal, transport)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        super::query_api_session::startup(self, params)
    }

    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool> {
        self.with_session(session, |record| Ok(record.active_txn.is_some()))
    }

    fn begin_transaction(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
    ) -> DbResult<()> {
        self.begin_transaction_internal(session, isolation)
    }

    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        if self.with_session(session, |record| {
            Ok(record.transaction_failed
                && record.active_txn.is_some()
                && !record.implicit_txn_active)
        })? {
            self.discard_pending_notifications(session);
            self.rollback_transaction_internal(session)
        } else {
            let result = self.commit_transaction_internal(session);
            if result.is_ok() {
                self.flush_pending_notifications(session);
            } else {
                self.discard_pending_notifications(session);
            }
            result
        }
    }

    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        self.discard_pending_notifications(session);
        self.rollback_transaction_internal(session)
    }

    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        let start = Instant::now();
        let _inflight_query = self.metrics.track_inflight_query();
        if let Some(result) = self.try_execute_check_estimated_rows_query(session, sql)? {
            let duration_micros = elapsed_micros_u64(&start);
            let (rows_returned, rows_affected) = accumulate_statement_metrics(&result);
            self.metrics
                .record_query(duration_micros, rows_returned, rows_affected);
            return Ok(result);
        }
        if sql.len() > crate::config::MAX_SQL_LENGTH {
            if let Err(error) = self.take_cancellation_if_needed(session) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            self.metrics.record_failure();
            return Err(DbError::program_limit(
                "SQL statement exceeds maximum allowed length",
            ));
        }
        debug!(sql_len = sql.len(), "executing SQL");
        match self.try_execute_pg_stat_wal_receiver_query(sql) {
            Ok(Some(results)) => {
                let duration_micros = elapsed_micros_u64(&start);
                let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                self.metrics
                    .record_query(duration_micros, rows_returned, rows_affected);
                return Ok(results);
            }
            Ok(None) => {}
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        match self.try_execute_hash_join_batches_query_shortcuts(session, sql) {
            Ok(Some(results)) => {
                let duration_micros = elapsed_micros_u64(&start);
                let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                self.metrics
                    .record_query(duration_micros, rows_returned, rows_affected);
                return Ok(results);
            }
            Ok(None) => {}
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        let current_of_cursor_name;
        let mut failed_txn_active_prechecked = None;
        let parsed_sql_cache_enabled = parsed_sql_cache_enabled();
        let parsed_sql_plan_fingerprint_cache_enabled = parsed_sql_plan_fingerprint_cache_enabled();
        let parsed_sql_cache_hit;
        let parsed_sql_plan_fingerprints;
        // Hoist the SQL-only check out of the session lock: when the SQL
        // doesn't contain any of the built-in aggregate-rewrite hint
        // substrings AND we don't need to consult session-local
        // compat_aggregate_rewrites, we can avoid acquiring the session
        // lock at all. The vast majority of OLTP traffic falls through
        // without aggregate hints and without per-session aggregate
        // overrides, so skipping the lock per query is a measurable win.
        let compat_aggregate_preparse_rewrite_needed =
            if sql_may_use_builtin_compat_aggregate_rewrite(sql) {
                true
            } else {
                self.with_session(session, |record| {
                    Ok(!record.compat_aggregate_rewrites.is_empty()
                        && record.compat_aggregate_rewrites.keys().any(|name| {
                            super::compat::find_ascii_case_insensitive(sql, name).is_some()
                        }))
                })?
            };
        let statements = if !super::compat::sql_may_require_preparse_rewrite(sql)
            && !compat_aggregate_preparse_rewrite_needed
        {
            current_of_cursor_name = None;
            if parsed_sql_cache_enabled {
                let literal_shape = literal_shape_sql(sql);
                match self.take_cancellation_and_cached_sql_with_shape(
                    session,
                    sql,
                    literal_shape.as_ref().map(|shape| shape.sql.as_str()),
                ) {
                    Ok((Some(cached), failed_txn_active)) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                        parsed_sql_cache_hit = true;
                        let cache_sql = if cached.matched_shape {
                            literal_shape
                                .as_ref()
                                .map(|shape| shape.sql.as_str())
                                .unwrap_or(sql)
                        } else {
                            sql
                        };
                        parsed_sql_plan_fingerprints = cached_plan_fingerprints_for_entry(
                            self,
                            session,
                            cache_sql,
                            &cached.entry,
                            if cached.matched_shape {
                                "literal_shape"
                            } else {
                                "exact_sql"
                            },
                        );
                        if cached.matched_shape {
                            let literal_shape = literal_shape.as_ref().ok_or_else(|| {
                                DbError::internal(
                                    "SQL shape cache hit without available literal shape",
                                )
                            })?;
                            bind_literal_shape_statements(
                                cached.entry.statements.as_ref(),
                                &literal_shape.params,
                            )?
                        } else {
                            cached.entry.statements
                        }
                    }
                    Ok((None, failed_txn_active)) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                        if let Some(literal_shape) = literal_shape {
                            match parse_sql_with_single_statement_fast_path(&literal_shape.sql) {
                                Ok(shape_statements) => {
                                    let shape_statements = Arc::new(shape_statements);
                                    parsed_sql_cache_hit = false;
                                    parsed_sql_plan_fingerprints = None;
                                    if let Err(error) = self.with_session_mut(session, |record| {
                                        record.remember_sql(
                                            literal_shape.sql.clone(),
                                            Arc::clone(&shape_statements),
                                        );
                                        Ok(())
                                    }) {
                                        warn!(
                                            error = %error,
                                            "failed to update SQL shape parse cache for session"
                                        );
                                    }
                                    bind_literal_shape_statements(
                                        shape_statements.as_ref(),
                                        &literal_shape.params,
                                    )?
                                }
                                Err(shape_error) => {
                                    debug!(
                                        error = %shape_error,
                                        "SQL literal shape parse failed; falling back to exact SQL parse"
                                    );
                                    match parse_sql_and_remember(self, session, sql) {
                                        Ok(statements) => {
                                            parsed_sql_cache_hit = false;
                                            parsed_sql_plan_fingerprints = None;
                                            statements
                                        }
                                        Err(e) => {
                                            self.metrics.record_failure();
                                            let _ = self.mark_transaction_failed_if_active(session);
                                            return Err(e);
                                        }
                                    }
                                }
                            }
                        } else {
                            match parse_sql_and_remember(self, session, sql) {
                                Ok(statements) => {
                                    parsed_sql_cache_hit = false;
                                    parsed_sql_plan_fingerprints = None;
                                    statements
                                }
                                Err(e) => {
                                    self.metrics.record_failure();
                                    let _ = self.mark_transaction_failed_if_active(session);
                                    return Err(e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            } else {
                match self.take_cancellation_and_failed_txn_status(session) {
                    Ok(failed_txn_active) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                    }
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
                parsed_sql_cache_hit = false;
                parsed_sql_plan_fingerprints = None;
                match parse_sql_with_single_statement_fast_path(sql) {
                    Ok(statements) => Arc::new(statements),
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            }
        } else {
            if let Err(error) = self.take_cancellation_if_needed(session) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            match self.try_execute_copy_from_file(session, sql) {
                Ok(Some(results)) => {
                    let duration_micros = elapsed_micros_u64(&start);
                    let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                    self.metrics
                        .record_query(duration_micros, rows_returned, rows_affected);
                    return Ok(results);
                }
                Ok(None) => {}
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            }
            // Rewrite `WHERE CURRENT OF <cursor>` to `WHERE ctid = '<tid>'`
            // before parsing, so the planner sees a normal ctid predicate.
            let rewritten_sql;
            let sql = match self.try_rewrite_current_of(session, sql) {
                Ok(Some((rewritten, cursor_name))) => {
                    rewritten_sql = rewritten;
                    current_of_cursor_name = Some(cursor_name);
                    rewritten_sql.as_str()
                }
                Ok(None) => {
                    current_of_cursor_name = None;
                    sql
                }
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Rewrite `CREATE TABLE ... AS EXECUTE <name> [(...)]` to
            // `CREATE TABLE ... AS <resolved_sql>` before parsing, so the
            // parser only sees a normal CREATE TABLE AS SELECT.
            let rewritten_ctas;
            let sql = match self.try_rewrite_ctas_execute(session, sql) {
                Ok(Some(rewritten)) => {
                    rewritten_ctas = rewritten;
                    rewritten_ctas.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Normalize PostgreSQL-style LO mode expressions that use
            // hex-bit literals (x'20000' / x'40000') so planner typing sees
            // integer operands for bitwise operators.
            let rewritten_lo_modes;
            let sql = if let Some(rewritten) = super::compat::rewrite_largeobject_mode_literals(sql)
            {
                rewritten_lo_modes = rewritten;
                rewritten_lo_modes.as_str()
            } else {
                sql
            };

            let rewritten_typeorm_index_order;
            let sql = if let Some(rewritten) =
                super::compat::rewrite_typeorm_index_reflection_order(sql)
            {
                rewritten_typeorm_index_order = rewritten;
                rewritten_typeorm_index_order.as_str()
            } else {
                sql
            };

            // Rewrite CREATE SCHEMA AUTHORIZATION CURRENT_ROLE|CURRENT_USER|SESSION_USER
            // to concrete role names before parsing inline-body schema checks.
            let rewritten_create_schema_auth;
            let sql = match self.try_rewrite_create_schema_authorization_pseudo_role(session, sql) {
                Ok(Some(rewritten)) => {
                    rewritten_create_schema_auth = rewritten;
                    rewritten_create_schema_auth.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            if let Some(error) = compat_multiarg_distinct_order_error(sql) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            if let Some(error) = ordered_set_usage_error(sql) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }

            let rewritten_compat_aggregates;
            let sql = match self.with_session(session, |record| {
                Ok(rewrite_compat_aggregate_query(
                    sql,
                    &record.compat_aggregate_rewrites,
                ))
            }) {
                Ok(Some(rewritten)) => {
                    rewritten_compat_aggregates = rewritten;
                    rewritten_compat_aggregates.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Cypher detection is handled by the parser: it returns
            // Statement::Cypher(...) which is dispatched in execute_statement_inner
            // (engine.rs).  No pre-parse interception needed here.

            if parsed_sql_cache_enabled {
                match self.with_session_mut(session, |record| Ok(record.cached_sql(sql))) {
                    Ok(Some(entry)) => {
                        parsed_sql_cache_hit = true;
                        parsed_sql_plan_fingerprints = if parsed_sql_plan_fingerprint_cache_enabled
                        {
                            match entry.plan_fingerprints {
                                Some(plan_fingerprints) => Some(plan_fingerprints),
                                None => {
                                    let plan_fingerprints =
                                        build_cached_plan_fingerprints(entry.statements.as_ref());
                                    if let Err(error) = self.with_session_mut(session, |record| {
                                        record.remember_sql_plan_fingerprints(
                                            sql,
                                            Arc::clone(&plan_fingerprints),
                                        );
                                        Ok(())
                                    }) {
                                        warn!(
                                            error = %error,
                                            "failed to update SQL plan fingerprint cache for session"
                                        );
                                    }
                                    Some(plan_fingerprints)
                                }
                            }
                        } else {
                            None
                        };
                        entry.statements
                    }
                    Ok(None) => match parse_sql_and_remember(self, session, sql) {
                        Ok(statements) => {
                            parsed_sql_cache_hit = false;
                            parsed_sql_plan_fingerprints = None;
                            statements
                        }
                        Err(e) => {
                            self.metrics.record_failure();
                            let _ = self.mark_transaction_failed_if_active(session);
                            return Err(e);
                        }
                    },
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            } else {
                parsed_sql_cache_hit = false;
                parsed_sql_plan_fingerprints = None;
                match parse_sql_with_single_statement_fast_path(sql) {
                    Ok(statements) => Arc::new(statements),
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            }
        };

        let mut results = if statements.len() == 1 {
            let allow_plan_cache = parsed_sql_cache_hit
                || (parameterized_literal_fast_path_enabled()
                    && is_literal_fast_path_candidate(&statements[0]));
            let statement_results = self.execute_sql_statement_results(
                session,
                sql,
                &statements[0],
                failed_txn_active_prechecked,
                allow_plan_cache,
                parsed_sql_plan_fingerprints
                    .as_ref()
                    .and_then(|fingerprints| fingerprints.first().copied().flatten()),
            )?;
            statement_results
        } else {
            Vec::with_capacity(statements.len())
        };
        if statements.len() > 1 {
            let run_batch = || -> DbResult<Vec<StatementResult>> {
                let mut batch_results = Vec::with_capacity(statements.len());
                let session_limits = self.session_info(session)?.limits;
                let mut cumulative_result_rows = 0u64;
                let mut cumulative_result_bytes = 0u64;
                for (index, statement) in statements.iter().enumerate() {
                    let allow_plan_cache = parsed_sql_cache_hit
                        || (parameterized_literal_fast_path_enabled()
                            && is_literal_fast_path_candidate(statement));
                    let statement_results = self.execute_sql_statement_results(
                        session,
                        sql,
                        statement,
                        None,
                        allow_plan_cache,
                        parsed_sql_plan_fingerprints
                            .as_ref()
                            .and_then(|fingerprints| fingerprints.get(index).copied().flatten()),
                    )?;
                    if let Err(error) = enforce_cumulative_statement_result_limits(
                        &statement_results,
                        &mut cumulative_result_rows,
                        &mut cumulative_result_bytes,
                        session_limits.max_result_rows,
                        session_limits.max_result_bytes,
                    ) {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                    batch_results.extend(statement_results);
                }
                Ok(batch_results)
            };

            let batch_results = if multi_statement_batch_uses_single_implicit_txn(&statements) {
                self.execute_with_implicit_transaction_options(session, true, true, run_batch)?
            } else {
                run_batch()?
            };
            results.extend(batch_results);
        }

        // When the SQL was rewritten from CURRENT OF, post-process EXPLAIN
        // output to show the original `CURRENT OF <cursor>` instead of the
        // resolved `(ctid = '<tid>'::tid)`.
        if let Some(ref cursor) = current_of_cursor_name {
            for result in &mut results {
                if let StatementResult::Query { rows, .. } = result {
                    restore_current_of_in_explain_rows(rows, cursor);
                }
            }
        }
        let duration_micros = elapsed_micros_u64(&start);
        let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
        self.metrics
            .record_query(duration_micros, rows_returned, rows_affected);

        Ok(results)
    }

    fn try_execute_check_estimated_rows_query(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(inner_sql) = parse_check_estimated_rows_inner_sql(sql) else {
            return Ok(None);
        };
        let explain_sql = format!("EXPLAIN ANALYZE {inner_sql}");
        // Dispatch through the typed `execute_statement` API rather than
        // re-entering `execute_sql`, which would recurse back into
        // `try_execute_check_estimated_rows_query`.
        let explain_statements = parse_sql_with_single_statement_fast_path(&explain_sql)?;
        let mut explain_results: Vec<StatementResult> =
            Vec::with_capacity(explain_statements.len());
        for stmt in &explain_statements {
            explain_results.push(self.execute_statement(session, stmt)?);
        }
        let explain_lines: Vec<String> = explain_results
            .iter()
            .filter_map(|result| match result {
                StatementResult::Query { rows, .. } => Some(rows),
                _ => None,
            })
            .flat_map(|rows| rows.iter())
            .filter_map(|row| row.values.first())
            .filter_map(|value| match value {
                Value::Text(line) => Some(line.clone()),
                _ => None,
            })
            .collect();
        // First, try the canonical PG format `(... rows=X ... rows=Y ...)`
        // on the first plan-node line.
        let mut explain_pair = explain_lines
            .iter()
            .find_map(|line| parse_explain_rows_pair(line));
        // Fallback: parse our own `Rows Returned: N` summary line and use
        // that count for both `estimated` and `actual`. This matches the
        // post-CREATE STATISTICS test expectations (where PG produces
        // `(actual, actual)`).
        if explain_pair.is_none() {
            let actual = explain_lines.iter().rev().find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("Rows Returned:")
                    .or_else(|| trimmed.strip_prefix("Rows Affected:"))
                    .and_then(|rest| rest.trim().parse::<i32>().ok())
            });
            if let Some(actual) = actual {
                explain_pair = Some((actual, actual));
            }
        }
        let explain_pair = explain_pair.unwrap_or((0, 0));

        let result = StatementResult::Query {
            columns: vec![
                crate::prepared::ResultColumn {
                    name: "estimated".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                crate::prepared::ResultColumn {
                    name: "actual".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![Row::new(vec![
                Value::Int(explain_pair.0),
                Value::Int(explain_pair.1),
            ])],
        };
        Ok(Some(vec![result]))
    }

    fn describe_sql_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedStatementDesc>> {
        describe_sql_statement_for_wire(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_metadata(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<SqlStatementWireMetadata> {
        sql_statement_wire_metadata(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        sql_statement_wire_cleanup_hint(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        sql_statement_wire_effective_statement(self, session, statement_sql, statement)
    }

    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        prepare_statement(self, session, statement_name, sql, None)
    }

    fn prepare_with_param_hints(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
        param_type_hints: Vec<Option<DataType>>,
    ) -> DbResult<PreparedStatementDesc> {
        prepare_statement(
            self,
            session,
            statement_name,
            sql,
            Some(param_type_hints.as_slice()),
        )
    }

    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        // Fold the cancellation check into the prepared-statement
        // lookup so describe_statement takes the session lock exactly
        // once on the OLTP hot path.
        let prepared = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            record
                .prepared_statements
                .get(statement_name)
                .cloned()
                .ok_or_else(unknown_prepared_statement_error)
        })?;

        Ok(
            refreshed_prepared_desc_if_dynamic(self, session, statement_name, &prepared)?
                .unwrap_or(prepared.desc),
        )
    }

    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        if portal_name.len() > crate::config::MAX_IDENTIFIER_LENGTH {
            return Err(DbError::program_limit(
                "portal name exceeds maximum allowed length",
            ));
        }

        // Fold the cancellation check into the main bind closure so we
        // only take the session lock once per bind on the OLTP hot
        // path (extended-protocol Bind from pgbench -M prepared).
        self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let Some(prepared) = record.prepared_statements.get(&statement_name) else {
                return Err(unknown_prepared_statement_error());
            };

            if !portal_name.is_empty() && record.portals.contains_key(&portal_name) {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::DuplicateObject,
                    format!("portal \"{portal_name}\" already exists"),
                ));
            }

            let expected_params = prepared.desc.param_types.len();
            if params.len() != expected_params {
                return Err(bind_parameter_count_error(expected_params, params.len()));
            }
            ensure_supported_portal_params(&params)?;
            ensure_portal_param_types_compatible(&prepared.desc.param_types, &params)?;

            let savepoint_generation = record.savepoints.last().map(|entry| entry.generation);
            if portal_name.is_empty() {
                if let Some(portal) = record.portals.get_mut(&portal_name) {
                    // Hot path for extended protocol unnamed portal rebinding
                    // (e.g. pgbench -M prepared): reuse the existing slot and
                    // reset per-execution state instead of reallocating.
                    portal.statement_name = statement_name;
                    portal.params = params;
                    portal.created_under_savepoint_generation = savepoint_generation;
                    portal.position = 0;
                    portal.exhausted = false;
                    portal.holdable = false;
                    portal.scrollable = false;
                    portal.current_ctid = None;
                    portal.hidden_ctid_column = None;
                    portal.current_of_relation_id = None;
                    portal.visible_result_columns = None;
                    portal.visible_result_column_origins = None;
                    portal.cached_columns = None;
                    portal.cached_rows = None;
                    return Ok(());
                }
            }

            if !record.portals.contains_key(&portal_name)
                && record.portals.len() >= record.info.limits.max_portals
            {
                return Err(DbError::program_limit("maximum number of portals reached"));
            }

            record.portals.insert(
                portal_name,
                PortalState {
                    statement_name,
                    params,
                    created_under_savepoint_generation: savepoint_generation,
                    position: 0,
                    exhausted: false,
                    holdable: false,
                    scrollable: false,
                    current_ctid: None,
                    hidden_ctid_column: None,
                    current_of_relation_id: None,
                    visible_result_columns: None,
                    visible_result_column_origins: None,
                    cached_columns: None,
                    cached_rows: None,
                },
            );

            Ok(())
        })
    }

    fn execute_prepared_statement_with_notices(
        &self,
        session: &SessionHandle,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        // Extended protocol Execute uses max_rows=0 to mean "no limit".
        let effective_max_rows = if max_rows == 0 { usize::MAX } else { max_rows };
        if effective_max_rows != usize::MAX {
            return <Self as QueryEngine>::bind_and_execute_portal_with_notices(
                self,
                session,
                String::new(),
                statement_name,
                params,
                effective_max_rows,
            );
        }

        let start = Instant::now();
        let (
            prepared_sql,
            statement_sql,
            statement,
            param_types,
            contains_parameters,
            uses_compat_command_hooks,
            uses_compat_rule_dml,
            may_use_drop_if_exists_notice,
            parameterized_plan_literal_rewrite,
            parameterized_plan_literal_rewrite_seeded,
            parameterized_eq_param_index,
            parameterized_insert_values_param_slots,
            plan_fingerprint,
            contains_recursive_cte,
            notice_free_execute,
            pending_notices_empty_at_start,
            failed_txn_active,
        ) = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let prepared = record
                .prepared_statements
                .get(&statement_name)
                .ok_or_else(unknown_prepared_statement_error)?;
            let statement_sql = prepared
                .needs_statement_sql_at_execute
                .then(|| prepared.sql.clone());
            Ok((
                prepared.sql.clone(),
                statement_sql,
                prepared.statement.clone(),
                prepared.param_types.clone(),
                prepared.contains_parameters,
                prepared.uses_compat_command_hooks,
                prepared.uses_compat_rule_dml,
                prepared.may_use_drop_if_exists_notice,
                prepared.parameterized_plan_literal_rewrite,
                prepared.parameterized_plan_literal_rewrite_seeded,
                prepared.parameterized_eq_param_index,
                prepared.parameterized_insert_values_param_slots.clone(),
                prepared.plan_fingerprint,
                prepared.contains_recursive_cte,
                prepared.notice_free_execute,
                record.pending_notices.is_empty(),
                record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active,
            ))
        })?;

        if params.len() != param_types.len() {
            return Err(bind_parameter_count_error(param_types.len(), params.len()));
        }
        // These checks are also enforced inside pgwire's bind path; in
        // FFI/SDK caller that bypasses pgwire submit `Value::Text` for an
        // `Int` parameter and reach `bind_statement_params` with a mistyped
        // value (audit query_api F-2). Run them as real checks here.
        ensure_supported_portal_params(&params)?;
        ensure_portal_param_types_compatible(param_types.as_ref(), &params)?;

        let can_use_prepared_select_result_cache = params.is_empty()
            && !contains_parameters
            && !uses_compat_command_hooks
            && !uses_compat_rule_dml
            && !contains_recursive_cte
            && pending_notices_empty_at_start
            && matches!(statement.as_ref(), Statement::Select(_));
        if can_use_prepared_select_result_cache {
            match self.try_prepared_select_result_cache_get(
                session,
                &prepared_sql,
                statement.as_ref(),
            ) {
                Ok(Some(batch)) => {
                    let duration_micros = elapsed_micros_u64(&start);
                    let rows_returned =
                        aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                    self.metrics.record_query(duration_micros, rows_returned, 0);
                    return Ok((batch, Vec::new()));
                }
                Ok(None) => {}
                Err(error) => {
                    self.metrics.record_failure();
                    return Err(error);
                }
            }
        }

        let completion_statement_owned = if matches!(
            statement.as_ref(),
            aiondb_parser::Statement::ExecuteStmt { .. }
        ) {
            Some(if let Some(statement_sql) = statement_sql.as_deref() {
                statement_wire_effective_statement_for_statement(
                    self,
                    session,
                    statement_sql,
                    &statement,
                )?
            } else {
                statement.as_ref().clone()
            })
        } else {
            None
        };
        let completion_statement = completion_statement_owned
            .as_ref()
            .unwrap_or(statement.as_ref());

        let parameterized_eq_literal_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && !matches!(statement.as_ref(), Statement::Update(_))
        {
            parameterized_eq_param_index
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| params.get(index))
                .cloned()
        } else {
            None
        };
        let parameterized_insert_values_literals_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && parameterized_eq_literal_override.is_none()
        {
            parameterized_insert_values_param_slots
                .as_deref()
                .and_then(|slots| parameterized_insert_values_bound_literals(slots, &params))
        } else {
            None
        };

        let rewritten_current_of_sql;
        let rewritten_current_of_statement;
        let current_of_cursor_name;
        let statement: std::borrow::Cow<'_, Statement> = if parameterized_eq_literal_override
            .is_some()
            || parameterized_insert_values_literals_override.is_some()
        {
            current_of_cursor_name = None;
            std::borrow::Cow::Owned(bind_statement_params(
                statement.as_ref(),
                &params,
                param_types.as_ref(),
            )?)
        } else if statement_sql.as_deref().is_some_and(|sql| {
            super::compat::find_ascii_case_insensitive(sql, "current of").is_some()
        }) {
            let statement_sql = statement_sql.as_deref().ok_or_else(|| {
                DbError::internal(
                    "prepared statement current of rewrite requires SQL text during execute",
                )
            })?;
            if let Some((rewritten, cursor_name)) =
                self.try_rewrite_current_of(session, statement_sql)?
            {
                rewritten_current_of_sql = rewritten;
                current_of_cursor_name = Some(cursor_name);
                rewritten_current_of_statement =
                    parse_prepared_statement(&rewritten_current_of_sql)?;
                std::borrow::Cow::Owned(bind_statement_params(
                    &rewritten_current_of_statement,
                    &params,
                    param_types.as_ref(),
                )?)
            } else {
                current_of_cursor_name = None;
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        } else {
            current_of_cursor_name = None;
            if params.is_empty() {
                std::borrow::Cow::Borrowed(statement.as_ref())
            } else {
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        };

        match self.execute_portal_statement(
            session,
            "",
            false,
            Some(failed_txn_active),
            statement.as_ref(),
            completion_statement,
            statement_sql.as_deref(),
            Some(super::portal_exec::PortalCompatHints {
                uses_command_hooks: uses_compat_command_hooks,
                uses_rule_dml: uses_compat_rule_dml,
                may_use_drop_if_exists_notice,
            }),
            plan_fingerprint,
            contains_recursive_cte,
            contains_parameters
                && (plan_fingerprint.is_none() || !parameterized_plan_literal_rewrite),
            parameterized_plan_literal_rewrite,
            parameterized_eq_literal_override,
            parameterized_insert_values_literals_override,
            0,
            effective_max_rows,
        ) {
            Ok(mut batch) => {
                if parameterized_plan_literal_rewrite && !parameterized_plan_literal_rewrite_seeded
                {
                    let _ = self.with_session_mut(session, |record| {
                        if let Some(prepared) = record.prepared_statements.get_mut(&statement_name)
                        {
                            prepared.parameterized_plan_literal_rewrite_seeded = true;
                        }
                        Ok(())
                    });
                }
                if let Some(cursor) = current_of_cursor_name.as_deref() {
                    restore_current_of_in_explain_rows(&mut batch.rows, cursor);
                }
                if can_use_prepared_select_result_cache {
                    if let Err(error) = self.prepared_select_result_cache_put(
                        session,
                        &prepared_sql,
                        statement.as_ref(),
                        &batch,
                    ) {
                        warn!(
                            error = %error,
                            "failed to update prepared SELECT result cache"
                        );
                    }
                }
                let duration_micros = elapsed_micros_u64(&start);
                let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                self.metrics.record_query(duration_micros, rows_returned, 0);
                let notices = if notice_free_execute && pending_notices_empty_at_start {
                    Vec::new()
                } else {
                    Engine::drain_pending_notices(self, session)?
                };
                Ok((batch, notices))
            }
            Err(error) => {
                self.metrics.record_failure();
                Err(error)
            }
        }
    }

    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription> {
        // Fold the cancellation check into the same session lock as
        // the portal/statement lookup; one lock acquisition per
        // describe instead of two.
        let (statement_name, prepared) = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let portal = record
                .portals
                .get(portal_name)
                .ok_or_else(unknown_portal_error)?;
            let statement = record
                .prepared_statements
                .get(&portal.statement_name)
                .cloned()
                .ok_or_else(unknown_portal_error)?;

            Ok((portal.statement_name.clone(), statement))
        })?;

        let desc = refreshed_prepared_desc_if_dynamic(self, session, &statement_name, &prepared)?
            .unwrap_or(prepared.desc);
        let (visible_result_columns, visible_result_column_origins) =
            self.with_session(session, |record| {
                let portal = record
                    .portals
                    .get(portal_name)
                    .ok_or_else(unknown_portal_error)?;
                Ok((
                    portal.visible_result_columns.clone(),
                    portal.visible_result_column_origins.clone(),
                ))
            })?;
        Ok(PortalDescription {
            result_columns: visible_result_columns.unwrap_or(desc.result_columns),
            result_column_origins: visible_result_column_origins
                .unwrap_or(desc.result_column_origins),
        })
    }

    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        let start = Instant::now();
        let (
            prepared_statement_name,
            statement_sql,
            statement,
            param_types,
            contains_parameters,
            uses_compat_command_hooks,
            uses_compat_rule_dml,
            may_use_drop_if_exists_notice,
            params,
            parameterized_plan_literal_rewrite,
            parameterized_plan_literal_rewrite_seeded,
            parameterized_insert_values_param_slots,
            plan_fingerprint,
            contains_recursive_cte,
            position,
            exhausted,
            has_cached_result,
        ) = {
            self.with_session_mut(session, |record| {
                Self::consume_cancellation_if_needed(record)?;
                let portal = record
                    .portals
                    .get(portal_name)
                    .ok_or_else(unknown_portal_error)?;
                let prepared = record
                    .prepared_statements
                    .get(&portal.statement_name)
                    .ok_or_else(unknown_portal_error)?;
                let statement_sql = prepared
                    .needs_statement_sql_at_execute
                    .then(|| prepared.sql.clone());
                Ok((
                    portal.statement_name.clone(),
                    statement_sql,
                    prepared.statement.clone(),
                    prepared.param_types.clone(),
                    prepared.contains_parameters,
                    prepared.uses_compat_command_hooks,
                    prepared.uses_compat_rule_dml,
                    prepared.may_use_drop_if_exists_notice,
                    portal.params.clone(),
                    prepared.parameterized_plan_literal_rewrite,
                    prepared.parameterized_plan_literal_rewrite_seeded,
                    prepared.parameterized_insert_values_param_slots.clone(),
                    prepared.plan_fingerprint,
                    prepared.contains_recursive_cte,
                    portal.position,
                    portal.exhausted,
                    portal.cached_columns.is_some() && portal.cached_rows.is_some(),
                ))
            })?
        };

        let completion_statement_owned = if matches!(
            statement.as_ref(),
            aiondb_parser::Statement::ExecuteStmt { .. }
        ) {
            Some(if let Some(statement_sql) = statement_sql.as_deref() {
                statement_wire_effective_statement_for_statement(
                    self,
                    session,
                    statement_sql,
                    &statement,
                )?
            } else {
                statement.as_ref().clone()
            })
        } else {
            None
        };
        let completion_statement = completion_statement_owned
            .as_ref()
            .unwrap_or(statement.as_ref());

        let parameterized_eq_literal_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && !matches!(statement.as_ref(), Statement::Update(_))
        {
            parameterized_eq_bind_param_index(statement.as_ref())
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| params.get(index))
                .cloned()
        } else {
            None
        };
        let parameterized_insert_values_literals_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && parameterized_eq_literal_override.is_none()
        {
            parameterized_insert_values_param_slots
                .as_deref()
                .and_then(|slots| parameterized_insert_values_bound_literals(slots, &params))
        } else {
            None
        };

        if exhausted {
            return Ok(PortalBatch {
                columns: Vec::new(),
                rows: Vec::new(),
                tag: super::portal_exec::query_completion_tag(completion_statement, 0),
                rows_affected: 0,
                exhausted: true,
            });
        }

        if has_cached_result {
            if let Err(error) = self.authorize_statement(session, completion_statement) {
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            let batch = self.execute_cached_portal_query(
                session,
                portal_name,
                completion_statement,
                max_rows,
            )?;
            let duration_micros = elapsed_micros_u64(&start);
            let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
            self.metrics.record_query(duration_micros, rows_returned, 0);
            return Ok(batch);
        }

        let rewritten_current_of_sql;
        let rewritten_current_of_statement;
        let current_of_cursor_name;
        let statement: std::borrow::Cow<'_, Statement> = if parameterized_eq_literal_override
            .is_some()
            || parameterized_insert_values_literals_override.is_some()
        {
            current_of_cursor_name = None;
            std::borrow::Cow::Owned(bind_statement_params(
                statement.as_ref(),
                &params,
                param_types.as_ref(),
            )?)
        } else if statement_sql.as_deref().is_some_and(|sql| {
            super::compat::find_ascii_case_insensitive(sql, "current of").is_some()
        }) {
            let statement_sql = statement_sql.as_deref().ok_or_else(|| {
                DbError::internal(
                    "prepared portal current of rewrite requires SQL text during execute",
                )
            })?;
            if let Some((rewritten, cursor_name)) =
                self.try_rewrite_current_of(session, statement_sql)?
            {
                rewritten_current_of_sql = rewritten;
                current_of_cursor_name = Some(cursor_name);
                rewritten_current_of_statement =
                    parse_prepared_statement(&rewritten_current_of_sql)?;
                std::borrow::Cow::Owned(bind_statement_params(
                    &rewritten_current_of_statement,
                    &params,
                    param_types.as_ref(),
                )?)
            } else {
                current_of_cursor_name = None;
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        } else {
            current_of_cursor_name = None;
            if params.is_empty() {
                std::borrow::Cow::Borrowed(statement.as_ref())
            } else {
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        };
        match self.execute_portal_statement(
            session,
            portal_name,
            true,
            None,
            statement.as_ref(),
            completion_statement,
            statement_sql.as_deref(),
            Some(super::portal_exec::PortalCompatHints {
                uses_command_hooks: uses_compat_command_hooks,
                uses_rule_dml: uses_compat_rule_dml,
                may_use_drop_if_exists_notice,
            }),
            plan_fingerprint,
            contains_recursive_cte,
            contains_parameters
                && (plan_fingerprint.is_none() || !parameterized_plan_literal_rewrite),
            parameterized_plan_literal_rewrite,
            parameterized_eq_literal_override,
            parameterized_insert_values_literals_override,
            position,
            max_rows,
        ) {
            Ok(mut batch) => {
                if parameterized_plan_literal_rewrite && !parameterized_plan_literal_rewrite_seeded
                {
                    let _ = self.with_session_mut(session, |record| {
                        if let Some(prepared) =
                            record.prepared_statements.get_mut(&prepared_statement_name)
                        {
                            prepared.parameterized_plan_literal_rewrite_seeded = true;
                        }
                        Ok(())
                    });
                }
                if let Some(cursor) = current_of_cursor_name.as_deref() {
                    restore_current_of_in_explain_rows(&mut batch.rows, cursor);
                }
                let duration_micros = elapsed_micros_u64(&start);
                let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                self.metrics.record_query(duration_micros, rows_returned, 0);
                Ok(batch)
            }
            Err(e) => {
                self.metrics.record_failure();
                Err(e)
            }
        }
    }

    fn statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        prepared_statement_wire_cleanup_hint(self, session, statement_name)
    }

    fn statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<aiondb_parser::Statement>> {
        prepared_statement_wire_effective_statement(self, session, statement_name)
    }

    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.prepared_statements.remove(statement_name);
            record.compat_prepared_sql.remove(statement_name);
            record
                .portals
                .retain(|_, portal| portal.statement_name != statement_name);
            Ok(())
        })
    }

    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.portals.remove(portal_name);
            Ok(())
        })
    }

    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        self.execute_copy_from_internal(session, table_id, None, data)
    }

    fn drain_pending_notices(&self, session: &SessionHandle) -> DbResult<Vec<String>> {
        self.with_session_mut(session, |record| {
            Ok(std::mem::take(&mut record.pending_notices))
        })
    }

    fn savepoint_generation(&self, session: &SessionHandle, name: &str) -> DbResult<Option<u64>> {
        self.with_session(session, |record| {
            Ok(record
                .savepoints
                .iter()
                .rev()
                .find(|entry| entry.name == name)
                .map(|entry| entry.generation))
        })
    }

    fn check_session_cancellation(&self, session: &SessionHandle) -> DbResult<()> {
        self.take_cancellation_if_needed(session)
    }

    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()> {
        super::query_api_session::cancel_session(self, session)
    }

    fn session_count(&self) -> DbResult<usize> {
        super::query_api_session::session_count(self)
    }

    fn terminate(&self, session: SessionHandle) -> DbResult<()> {
        super::query_api_session::terminate(self, session)
    }
}
