//! SQL statement classification, parameterized/literal rewrite detection,
//! plan-fingerprinting and literal-shape fast-path helpers.
//!
//! Split out of `query_api.rs` (the pre-`impl` free-fn block).
//! Parent/engine scope reached via `use super::*`.
#![allow(clippy::pedantic, clippy::too_many_lines, clippy::wildcard_imports)]

use super::*;

pub(in crate::engine) const FAILED_TRANSACTION_MESSAGE: &str =
    "current transaction is aborted, commands ignored until end of transaction block";

pub(in crate::engine) use super::query_api_copy_compat::{
    decode_sql_single_quoted_literal, escape_copy_text_value, format_copy_csv_value,
    format_copy_text_value, object_name_to_qualified, split_top_level_csv_items,
    unescape_copy_text_value, CopyColumnCompat, CopyCompatFormat, CopyCompatOptions, CopyCsvField,
    CopyWhereOp, CopyWherePredicate,
};

pub(in crate::engine) fn extract_option_numeric_literal(sql: &str, key: &str) -> Option<String> {
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

pub(in crate::engine) fn validate_brin_bloom_index_options(statement: &Statement, sql: &str) -> DbResult<()> {
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

pub(in crate::engine) fn extract_hash_join_batches_arg(sql: &str) -> Option<String> {
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

pub(in crate::engine) fn can_reuse_cached_plan_fingerprint(statement: &Statement) -> bool {
    Engine::cacheable_plan_statement(statement)
        && !statement_contains_parameters(statement)
        && !super::recursive_cte::statement_contains_recursive_cte(statement)
        && !super::statement_policy::statement_requires_acl_normalization(statement)
}

pub(in crate::engine) fn parser_expr_strip_casts(expr: &aiondb_parser::Expr) -> &aiondb_parser::Expr {
    let mut current = expr;
    while let aiondb_parser::Expr::Cast { expr, .. } = current {
        current = expr;
    }
    current
}

pub(in crate::engine) fn parser_expr_is_parameter(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Parameter { .. }
    )
}

pub(in crate::engine) fn parser_expr_is_identifier(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Identifier(_)
    )
}

// `sql_contains_ascii_case_insensitive` lives in
// `engine/compat/router_helpers.rs`.

// directly by the compat cascade below via

pub(in crate::engine) fn parser_expr_is_literal(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        parser_expr_strip_casts(expr),
        aiondb_parser::Expr::Literal(_, _)
    )
}

pub(in crate::engine) fn parser_object_name_single_part_matches(
    name: &aiondb_parser::ObjectName,
    expected: &str,
) -> bool {
    name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case(expected)
}

pub(in crate::engine) fn parser_expr_is_insert_fast_path_stable_value(expr: &aiondb_parser::Expr) -> bool {
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

pub(in crate::engine) fn parser_expr_parameter_index(expr: &aiondb_parser::Expr) -> Option<usize> {
    match parser_expr_strip_casts(expr) {
        aiondb_parser::Expr::Parameter { index, .. } => usize::try_from(*index).ok(),
        _ => None,
    }
}

pub(in crate::engine) fn parser_binary_op_is_arith(op: aiondb_parser::BinaryOperator) -> bool {
    matches!(
        op,
        aiondb_parser::BinaryOperator::Add
            | aiondb_parser::BinaryOperator::Sub
            | aiondb_parser::BinaryOperator::Mul
            | aiondb_parser::BinaryOperator::Div
            | aiondb_parser::BinaryOperator::Mod
    )
}

pub(in crate::engine) fn statement_matches_parameterized_select_eq_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn statement_matches_parameterized_update_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn statement_matches_parameterized_delete_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn statement_matches_parameterized_insert_values_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn parameterized_insert_values_param_slots(
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

pub(in crate::engine) fn parameterized_insert_values_bound_literals(
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

pub(in crate::engine) fn statement_matches_literal_select_eq_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn parser_binary_op_is_comparison(op: aiondb_parser::BinaryOperator) -> bool {
    matches!(
        op,
        aiondb_parser::BinaryOperator::Eq
            | aiondb_parser::BinaryOperator::Ge
            | aiondb_parser::BinaryOperator::Gt
            | aiondb_parser::BinaryOperator::Le
            | aiondb_parser::BinaryOperator::Lt
    )
}

pub(in crate::engine) fn selection_matches_literal_select_rewrite(selection: &aiondb_parser::Expr) -> bool {
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

pub(in crate::engine) fn statement_matches_literal_update_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn statement_matches_literal_delete_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn statement_matches_literal_insert_values_rewrite(statement: &Statement) -> bool {
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

pub(in crate::engine) fn canonicalize_literal(literal: &aiondb_parser::Literal) -> aiondb_parser::Literal {
    match literal {
        aiondb_parser::Literal::Integer(_) => aiondb_parser::Literal::Integer(0),
        aiondb_parser::Literal::NumericLit(_) => aiondb_parser::Literal::NumericLit("0".to_owned()),
        aiondb_parser::Literal::String(_) => aiondb_parser::Literal::String(String::new()),
        aiondb_parser::Literal::Boolean(_) => aiondb_parser::Literal::Boolean(false),
        aiondb_parser::Literal::Null => aiondb_parser::Literal::Null,
    }
}

pub(in crate::engine) fn canonicalize_literal_side(expr: &aiondb_parser::Expr) -> Option<aiondb_parser::Expr> {
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

pub(in crate::engine) fn canonicalize_select_filter_literals(expr: &mut aiondb_parser::Expr) -> Option<()> {
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

pub(in crate::engine) fn canonicalize_assignment_binary_literal(expr: &mut aiondb_parser::Expr) -> Option<()> {
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

pub(in crate::engine) fn is_literal_fast_path_candidate(statement: &Statement) -> bool {
    Engine::cacheable_plan_statement(statement)
        && !super::recursive_cte::statement_contains_recursive_cte(statement)
        && !super::statement_policy::statement_requires_acl_normalization(statement)
        && (statement_matches_literal_select_eq_rewrite(statement)
            || statement_matches_literal_update_rewrite(statement)
            || statement_matches_literal_delete_rewrite(statement)
            || statement_matches_literal_insert_values_rewrite(statement))
}

pub(in crate::engine) fn literal_fast_path_plan_fingerprint(
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

pub(in crate::engine) fn parser_expr_contains_parameter(expr: &aiondb_parser::Expr) -> bool {
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

pub(in crate::engine) fn can_use_parameterized_plan_literal_rewrite(statement: &Statement) -> bool {
    statement_matches_parameterized_select_eq_rewrite(statement)
        || statement_matches_parameterized_update_rewrite(statement)
        || statement_matches_parameterized_delete_rewrite(statement)
        || statement_matches_parameterized_insert_values_rewrite(statement)
}

pub(in crate::engine) fn multi_statement_batch_uses_single_implicit_txn(statements: &[Statement]) -> bool {
    statements.len() > 1
        && statements
            .iter()
            .all(statement_requires_implicit_transaction)
}

pub(in crate::engine) fn parameterized_eq_bind_param_index(statement: &Statement) -> Option<usize> {
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

pub(in crate::engine) fn prepared_statement_needs_sql_at_execute(statement: &Statement, statement_sql: &str) -> bool {
    super::compat::statement_tracks_compat_types(statement)
        || super::compat::statement_uses_compat_command_hooks_with_sql(statement, statement_sql)
        || super::compat::statement_is_planner_pg_object_command(statement)
        || super::compat::statement_may_use_drop_if_exists_notice(statement)
        || super::compat::find_ascii_case_insensitive(statement_sql, "current of").is_some()
}

pub(in crate::engine) fn is_notice_free_expr(expr: &aiondb_parser::Expr) -> bool {
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

pub(in crate::engine) fn select_statement_is_notice_free(select: &aiondb_parser::SelectStatement) -> bool {
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

pub(in crate::engine) fn statement_is_notice_free_for_execute(statement: &Statement) -> bool {
    match statement {
        Statement::Select(select) => select_statement_is_notice_free(select),
        _ => false,
    }
}

pub(in crate::engine) fn build_cached_plan_fingerprints(
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

pub(in crate::engine) fn cached_plan_fingerprints_for_entry(
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

pub(in crate::engine) fn parse_sql_with_single_statement_fast_path(sql: &str) -> DbResult<Vec<Statement>> {
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

pub(in crate::engine) fn parsed_sql_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("AIONDB_ENGINE_DISABLE_PARSED_SQL_CACHE").is_none())
}

pub(in crate::engine) fn parsed_sql_plan_fingerprint_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("AIONDB_ENGINE_DISABLE_PARSED_SQL_FINGERPRINT_CACHE").is_none()
    })
}

pub(in crate::engine) struct LiteralShapeSql {
    pub(in crate::engine) sql: String,
    pub(in crate::engine) params: Vec<Value>,
}

pub(in crate::engine) fn literal_shape_parse_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("AIONDB_ENGINE_DISABLE_LITERAL_SHAPE_PARSE_CACHE").is_none()
    })
}

pub(in crate::engine) fn literal_shape_sql(sql: &str) -> Option<LiteralShapeSql> {
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

pub(in crate::engine) fn literal_shape_statement_kind_supported(trimmed_sql: &str) -> bool {
    let first_word = trimmed_sql
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .next()
        .unwrap_or_default();
    matches!(
        first_word.to_ascii_lowercase().as_str(),
        "select" | "insert" | "update" | "delete"
    )
}

pub(in crate::engine) fn literal_shape_is_constant_select(trimmed_sql: &str) -> bool {
    let lower = trimmed_sql.to_ascii_lowercase();
    lower.starts_with("select") && !lower.split_ascii_whitespace().any(|word| word == "from")
}

pub(in crate::engine) fn push_literal_shape_param(out: &mut String, params: &mut Vec<Value>, value: Value) -> Option<()> {
    if params.len() >= 128 {
        return None;
    }
    params.push(value);
    out.push('$');
    out.push_str(&params.len().to_string());
    Some(())
}

pub(in crate::engine) fn parse_single_quoted_literal(sql: &str, start: usize) -> Option<(String, usize)> {
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

pub(in crate::engine) fn copy_double_quoted_identifier(sql: &str, start: usize, out: &mut String) -> Option<usize> {
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

pub(in crate::engine) fn parse_integer_literal(sql: &str, start: usize) -> Option<(Value, usize)> {
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

pub(in crate::engine) fn previous_allows_numeric_literal(bytes: &[u8], index: usize) -> bool {
    index == 0
        || bytes[index - 1].is_ascii_whitespace()
        || matches!(
            bytes[index - 1],
            b'(' | b',' | b'=' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%'
        )
}

pub(in crate::engine) fn is_sql_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

pub(in crate::engine) fn bind_literal_shape_statements(
    statements: &[Statement],
    params: &[Value],
) -> DbResult<Arc<Vec<Statement>>> {
    let mut bound = Vec::with_capacity(statements.len());
    for statement in statements {
        bound.push(bind_statement_params(statement, params, &[])?);
    }
    Ok(Arc::new(bound))
}

pub(in crate::engine) fn parameterized_literal_fast_path_enabled() -> bool {
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

pub(in crate::engine) fn prepared_select_result_cache_sql_eligible(sql: &str, statement: &Statement) -> bool {
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

pub(in crate::engine) fn format_pg_lsn_text(lsn: aiondb_wal::Lsn) -> String {
    let raw = lsn.get();
    let lower = u32::try_from(raw & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    format!("{:X}/{:08X}", raw >> 32, lower)
}

/// Mask `password=`/`passfile=`/`sslpassword=` tokens in a libpq conninfo
/// string before surfacing it to non-superusers. Mirrors PostgreSQL's
/// behaviour for `pg_stat_wal_receiver.conninfo` outside the
/// `pg_read_all_stats` role. The parser walks the libpq quoting grammar so
/// values containing whitespace inside `'...'` are not split on spaces.
pub(in crate::engine) fn redact_libpq_conninfo_secrets(conninfo: &str) -> String {
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
