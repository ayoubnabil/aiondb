//! Parsers and SQL reconstructors for PostgreSQL-compatible rule syntax
//! (`CREATE RULE`, `DROP RULE`) and the expression/statement printers used
//! by the engine when it needs to emit pg-compatible error messages or
//! rewrite text without re-lexing.
//!
//! Everything in this module is pure: operates on `&str` or
//! `aiondb_parser` AST nodes and returns plain data.

use aiondb_core::DbError;

use crate::scan::{
    consume_word_ci, parse_compat_identifier, skip_sql_whitespace, strip_compat_word_ci,
    trim_compat_statement,
};
use crate::WITH_DML_RULE_ERROR_PREFIX;

pub fn non_scroll_cursor_backward_error() -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::ObjectNotInPrerequisiteState,
        "cursor can only scan forward",
    )
    .with_client_hint("Declare it with SCROLL option to enable backward scan.")
}

pub fn rewrite_rule_action_with_new_values(
    action_sql: &str,
    col_values: &[(String, String)],
) -> String {
    let mut sql = action_sql.to_owned();

    let new_star_replacement = col_values
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut cursor = 0usize;
    loop {
        let lower = sql[cursor..].to_ascii_lowercase();
        let Some(relative_pos) = lower.find("new.*") else {
            break;
        };
        let pos = cursor + relative_pos;
        sql.replace_range(pos..pos + 5, &new_star_replacement);
        cursor = pos.saturating_add(new_star_replacement.len());
    }

    for (col_name, value_sql) in col_values {
        let pattern = format!("new.{col_name}");
        sql = case_insensitive_replace_identifier(&sql, &pattern, value_sql);
    }

    sql
}

pub fn rewrite_rule_action_with_old_new_values(
    action_sql: &str,
    column_names: &[String],
    old_values: &[aiondb_core::Value],
    new_values: &[aiondb_core::Value],
) -> String {
    let mut sql = action_sql.to_owned();

    let old_star_replacement = old_values
        .iter()
        .map(sql_value_literal)
        .collect::<Vec<_>>()
        .join(", ");
    sql = case_insensitive_replace_identifier(&sql, "old.*", &old_star_replacement);

    let new_star_replacement = new_values
        .iter()
        .map(sql_value_literal)
        .collect::<Vec<_>>()
        .join(", ");
    sql = case_insensitive_replace_identifier(&sql, "new.*", &new_star_replacement);

    for (index, column_name) in column_names.iter().enumerate() {
        let old_literal = old_values
            .get(index)
            .map(sql_value_literal)
            .unwrap_or_else(|| "NULL".to_owned());
        let new_literal = new_values
            .get(index)
            .map(sql_value_literal)
            .unwrap_or_else(|| "NULL".to_owned());
        sql =
            case_insensitive_replace_identifier(&sql, &format!("old.{column_name}"), &old_literal);
        sql =
            case_insensitive_replace_identifier(&sql, &format!("new.{column_name}"), &new_literal);
    }

    sql
}

pub fn encode_with_dml_rule_marker(message: &str, action_sql: Option<&str>) -> String {
    match action_sql {
        Some(action_sql) if !action_sql.trim().is_empty() => {
            format!(
                "{WITH_DML_RULE_ERROR_PREFIX}{message}\n{}",
                action_sql.trim()
            )
        }
        _ => format!("{WITH_DML_RULE_ERROR_PREFIX}{message}"),
    }
}

pub fn split_with_dml_rule_marker_payload(payload: &str) -> (&str, Option<&str>) {
    if let Some((message, action_sql)) = payload.split_once('\n') {
        let action_sql = action_sql.trim();
        if action_sql.is_empty() {
            (message.trim(), None)
        } else {
            (message.trim(), Some(action_sql))
        }
    } else {
        (payload.trim(), None)
    }
}

pub fn is_supported_update_transition_values_action(action_sql: &str) -> bool {
    let lower = action_sql.to_ascii_lowercase();
    lower.contains("values(") && lower.contains("old.*") && lower.contains("new.*")
}

pub fn sql_value_literal(value: &aiondb_core::Value) -> String {
    use aiondb_core::Value;

    match value {
        Value::Null => "NULL".to_owned(),
        Value::Int(number) => number.to_string(),
        Value::BigInt(number) => number.to_string(),
        Value::Real(number) => format_float_literal(f64::from(*number)),
        Value::Double(number) => format_float_literal(*number),
        Value::Numeric(number) => number.to_string(),
        Value::Money(number) => format!(
            "CAST('{}' AS MONEY)",
            aiondb_core::escape_sql_literal(&Value::Money(*number).to_string())
        ),
        Value::Text(text) => format!("'{}'", aiondb_core::escape_sql_literal(text)),
        Value::Boolean(value) => {
            if *value {
                "TRUE".to_owned()
            } else {
                "FALSE".to_owned()
            }
        }
        Value::Blob(bytes) => format!("'\\x{}'", aiondb_core::hex_encode(bytes)),
        Value::Timestamp(timestamp) => format!("CAST('{timestamp}' AS TIMESTAMP)"),
        Value::Date(date) => format!("CAST('{date}' AS DATE)"),
        Value::LargeDate(date) => format!("CAST('{date}' AS DATE)"),
        Value::Time(time) => format!("CAST('{time}' AS TIME)"),
        Value::TimeTz(time, offset) => format!("CAST('{time}{offset}' AS TIMETZ)"),
        Value::Interval(interval) => format!(
            "CAST('{} months {} days {} microseconds' AS INTERVAL)",
            interval.months, interval.days, interval.micros
        ),
        Value::Uuid(bytes) => format!("CAST('{}' AS UUID)", aiondb_core::Value::Uuid(*bytes)),
        Value::TimestampTz(timestamp) => format!("CAST('{timestamp}' AS TIMESTAMPTZ)"),
        Value::Tid(value) => format!("CAST('{value}' AS TID)"),
        Value::PgLsn(value) => format!("CAST('{value}' AS PG_LSN)"),
        Value::Jsonb(json) => format!(
            "CAST('{}' AS JSONB)",
            aiondb_core::escape_sql_literal(&json.to_string())
        ),
        Value::MacAddr(value) => format!("CAST('{value}' AS MACADDR)"),
        Value::MacAddr8(value) => format!("CAST('{value}' AS MACADDR8)"),
        Value::Vector(vector) => {
            let values = vector
                .values
                .iter()
                .map(|entry| entry.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("CAST('[{values}]' AS VECTOR({}))", vector.dims)
        }
        Value::Array(values) => {
            let elements = values
                .iter()
                .map(sql_value_literal)
                .collect::<Vec<_>>()
                .join(", ");
            format!("ARRAY[{elements}]")
        }
    }
}

pub fn format_float_literal(value: f64) -> String {
    if value.is_nan() {
        "CAST('NaN' AS DOUBLE)".to_owned()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "CAST('Infinity' AS DOUBLE)".to_owned()
        } else {
            "CAST('-Infinity' AS DOUBLE)".to_owned()
        }
    } else {
        let literal = value.to_string();
        if literal.contains('.') || literal.contains('e') || literal.contains('E') {
            literal
        } else {
            format!("{literal}.0")
        }
    }
}

const RULE_NAME_REGISTRY_PREFIX: &str = "__aiondb_rule_name_registry__.";

pub struct ParsedAlterRuleRename {
    pub rule: String,
    pub relation: String,
    pub new_rule: String,
}

pub fn parse_create_rule_name(sql: &str) -> Option<String> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "CREATE")?;
    if consume_word_ci(sql, &mut cursor, "OR").is_some() {
        consume_word_ci(sql, &mut cursor, "REPLACE")?;
    }
    consume_word_ci(sql, &mut cursor, "RULE")?;
    parse_compat_identifier(sql, &mut cursor)
}

pub fn parse_alter_rule_rename(sql: &str) -> Option<ParsedAlterRuleRename> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "ALTER")?;
    consume_word_ci(sql, &mut cursor, "RULE")?;
    let rule = parse_compat_identifier(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "ON")?;
    let mut relation = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut cursor)?;
        relation.push('.');
        relation.push_str(&relation_part);
    }
    consume_word_ci(sql, &mut cursor, "RENAME")?;
    consume_word_ci(sql, &mut cursor, "TO").or_else(|| consume_word_ci(sql, &mut cursor, "AS"))?;
    let new_rule = parse_compat_identifier(sql, &mut cursor)?;
    Some(ParsedAlterRuleRename {
        rule,
        relation,
        new_rule,
    })
}

pub fn rule_name_registry_relation_key(relation_name: &str) -> String {
    format!(
        "{RULE_NAME_REGISTRY_PREFIX}{}",
        relation_name.to_ascii_lowercase()
    )
}

pub fn rule_name_registry_name_key(rule_name: &str) -> String {
    rule_name.to_ascii_lowercase()
}

pub struct ParsedCreateRule {
    pub relation_name: String,
    pub event: String,
    pub do_instead: bool,
    pub action_sql: String,
    pub returning_count: usize,
    pub with_dml_unsupported_error: Option<&'static str>,
    pub with_query_transition_ref_error: Option<&'static str>,
    pub action_validation_error: Option<ParsedRuleActionValidationError>,
}

pub struct ParsedRuleActionValidationError {
    pub sqlstate: aiondb_core::SqlState,
    pub message: String,
    pub hint: Option<&'static str>,
}

pub struct ParsedDropRuleTarget {
    pub rule_name: String,
    pub relation_name: String,
    pub if_exists: bool,
}

pub fn parse_create_rule_sql(sql: &str) -> Option<ParsedCreateRule> {
    let sql = trim_compat_statement(sql);
    let upper = sql.to_ascii_uppercase();
    let event;
    let on_pos;
    if let Some(p) = upper.find(" ON INSERT TO ") {
        event = "INSERT";
        on_pos = p;
    } else if let Some(p) = upper.find(" ON UPDATE TO ") {
        event = "UPDATE";
        on_pos = p;
    } else if let Some(p) = upper.find(" ON DELETE TO ") {
        event = "DELETE";
        on_pos = p;
    } else {
        return None;
    }

    let after_to = on_pos + format!(" ON {event} TO ").len();
    let mut relation_cursor = after_to;
    let mut relation_name = parse_compat_identifier(sql, &mut relation_cursor)?;
    skip_sql_whitespace(sql, &mut relation_cursor);
    if sql
        .get(relation_cursor..)
        .is_some_and(|rest| rest.starts_with('.'))
    {
        relation_cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut relation_cursor)?;
        relation_name.push('.');
        relation_name.push_str(&relation_part);
    }

    let do_rel_pos = find_keyword_not_in_parens(&upper[after_to..], "DO")?;
    let do_abs_pos = after_to.saturating_add(do_rel_pos);
    let where_between = find_keyword_not_in_parens(&upper[after_to..do_abs_pos], "WHERE").is_some();

    let mut do_cursor = do_abs_pos;
    consume_word_ci(sql, &mut do_cursor, "DO")?;
    let do_clause = sql[do_cursor..].trim_start();
    let do_clause_upper = do_clause.to_ascii_uppercase();

    let (do_instead, action_sql, with_dml_unsupported_error) = if do_clause_upper
        .starts_with("INSTEAD")
    {
        let mut clause_cursor = 0usize;
        consume_word_ci(do_clause, &mut clause_cursor, "INSTEAD")?;
        let action_sql = do_clause[clause_cursor..].trim().to_owned();
        let action_upper = action_sql.to_ascii_uppercase();

        let unsupported = if where_between {
            Some(
                    "conditional DO INSTEAD rules are not supported for data-modifying statements in WITH",
                )
        } else if action_upper == "NOTHING" {
            Some("DO INSTEAD NOTHING rules are not supported for data-modifying statements in WITH")
        } else if action_upper == "NOTIFY" || action_upper.starts_with("NOTIFY ") {
            Some("DO INSTEAD NOTIFY rules are not supported for data-modifying statements in WITH")
        } else if action_sql.starts_with('(') && action_sql.contains(';') {
            Some(
                    "multi-statement DO INSTEAD rules are not supported for data-modifying statements in WITH",
                )
        } else {
            None
        };
        (true, action_sql, unsupported)
    } else if do_clause_upper.starts_with("ALSO") {
        let mut clause_cursor = 0usize;
        consume_word_ci(do_clause, &mut clause_cursor, "ALSO")?;
        (
            false,
            do_clause[clause_cursor..].trim().to_owned(),
            Some("DO ALSO rules are not supported for data-modifying statements in WITH"),
        )
    } else {
        return None;
    };

    let returning_count = if do_instead {
        count_returning_items(&action_sql)
    } else {
        0
    };

    let with_query_transition_ref_error = detect_with_query_transition_ref_error(&action_sql);

    let action_validation_error = detect_for_update_transition_relation_error(&action_sql)
        .map_or_else(
            || {
                detect_insert_values_single_identifier_error(&action_sql).map(|identifier| {
                    ParsedRuleActionValidationError {
                        sqlstate: aiondb_core::SqlState::UndefinedColumn,
                        message: format!("column \"{identifier}\" does not exist"),
                        hint: Some("Try using a table-qualified name."),
                    }
                })
            },
            |relation| {
                Some(ParsedRuleActionValidationError {
                    sqlstate: aiondb_core::SqlState::UndefinedTable,
                    message: format!(
                        "relation \"{relation}\" in FOR UPDATE clause not found in FROM clause"
                    ),
                    hint: None,
                })
            },
        );

    Some(ParsedCreateRule {
        relation_name,
        event: event.to_owned(),
        do_instead,
        action_sql,
        returning_count,
        with_dml_unsupported_error,
        with_query_transition_ref_error,
        action_validation_error,
    })
}

fn detect_for_update_transition_relation_error(action_sql: &str) -> Option<String> {
    let upper_sql = action_sql.to_ascii_uppercase();
    let mut scan_offset = 0usize;
    while scan_offset < upper_sql.len() {
        let rel_pos = find_keyword_not_in_parens(&upper_sql[scan_offset..], "FOR UPDATE OF")?;
        let mut cursor = scan_offset + rel_pos + "FOR UPDATE OF".len();
        loop {
            let relation_name = parse_compat_identifier(action_sql, &mut cursor)?;
            if relation_name.eq_ignore_ascii_case("old")
                || relation_name.eq_ignore_ascii_case("new")
            {
                return Some(relation_name.to_ascii_lowercase());
            }
            skip_sql_whitespace(action_sql, &mut cursor);
            if action_sql
                .get(cursor..)
                .is_some_and(|remaining| remaining.starts_with(','))
            {
                cursor += 1;
                continue;
            }
            break;
        }
        scan_offset = cursor.saturating_add(1);
    }
    None
}

fn detect_insert_values_single_identifier_error(action_sql: &str) -> Option<String> {
    let statement = aiondb_parser::parse_prepared_statement(action_sql).ok()?;
    let aiondb_parser::Statement::Insert(insert) = statement else {
        return None;
    };
    if insert.rows.len() != 1 {
        return None;
    }
    let row = insert.rows.first()?;
    if row.len() != 1 {
        return None;
    }
    let aiondb_parser::Expr::Identifier(identifier) = row.first()? else {
        return None;
    };
    if identifier.parts.len() != 1 {
        return None;
    }
    let name = identifier.parts.first()?.to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "current_user"
            | "session_user"
            | "current_role"
            | "user"
            | "current_date"
            | "current_time"
            | "current_timestamp"
            | "localtime"
            | "localtimestamp"
    ) {
        return None;
    }
    Some(name)
}

fn detect_with_query_transition_ref_error(action_sql: &str) -> Option<&'static str> {
    let sql = action_sql.trim_start();
    strip_compat_word_ci(sql, "WITH")?;

    let upper = sql.to_ascii_uppercase();
    let mut statement_start = None;
    for keyword in ["INSERT", "UPDATE", "DELETE", "SELECT", "VALUES"] {
        if let Some(pos) = find_keyword_not_in_parens(&upper, keyword) {
            if pos > 0 {
                statement_start =
                    Some(statement_start.map_or(pos, |current: usize| current.min(pos)));
            }
        }
    }
    let statement_start = statement_start?;

    let with_clause = &sql[..statement_start];
    if contains_transition_ref_with_dot(with_clause, "OLD") {
        return Some("cannot refer to OLD within WITH query");
    }
    if contains_transition_ref_with_dot(with_clause, "NEW") {
        return Some("cannot refer to NEW within WITH query");
    }
    None
}

fn contains_transition_ref_with_dot(haystack: &str, ident: &str) -> bool {
    let bytes = haystack.as_bytes();
    let ident_bytes = ident.as_bytes();
    if bytes.len() < ident_bytes.len() {
        return false;
    }

    for i in 0..=bytes.len() - ident_bytes.len() {
        if !bytes[i..i + ident_bytes.len()].eq_ignore_ascii_case(ident_bytes) {
            continue;
        }

        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }

        let mut j = i + ident_bytes.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'.' {
            return true;
        }
    }

    false
}

pub fn parse_drop_rule_target(sql: &str) -> Option<ParsedDropRuleTarget> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "DROP")?;
    consume_word_ci(sql, &mut cursor, "RULE")?;

    let if_exists = consume_word_ci(sql, &mut cursor, "IF")
        .and_then(|()| consume_word_ci(sql, &mut cursor, "EXISTS"));
    let if_exists = if_exists.is_some();

    let rule_name = parse_compat_identifier(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "ON")?;

    let mut relation_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut cursor)?;
        relation_name.push('.');
        relation_name.push_str(&relation_part);
    }
    Some(ParsedDropRuleTarget {
        rule_name,
        relation_name,
        if_exists,
    })
}

pub fn count_returning_items(sql: &str) -> usize {
    let upper = sql.to_ascii_uppercase();
    let Some(ret_pos) = find_keyword_not_in_parens(&upper, "RETURNING") else {
        return 0;
    };
    let returning_list = &sql[ret_pos + "RETURNING".len()..].trim();
    if returning_list.is_empty() {
        return 0;
    }
    let mut count = 1usize;
    let mut depth = 0i32;
    for ch in returning_list.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    if returning_list.trim() == "*" {
        return usize::MAX;
    }
    count
}

pub fn reconstruct_expr_sql(expr: &aiondb_parser::Expr) -> String {
    use aiondb_parser::Expr;
    match expr {
        Expr::Identifier(name) => name.parts.join("."),
        Expr::Literal(lit, _span) => match lit {
            aiondb_parser::Literal::Integer(n) => n.to_string(),
            aiondb_parser::Literal::NumericLit(s) => s.clone(),
            aiondb_parser::Literal::String(s) => format!("'{}'", s.replace('\'', "''")),
            aiondb_parser::Literal::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            aiondb_parser::Literal::Null => "NULL".to_owned(),
        },
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::Default { .. } => "DEFAULT".to_owned(),
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            let func_name = name.parts.join(".");
            let args_sql: Vec<String> = args.iter().map(reconstruct_expr_sql).collect();
            let distinct_str = if *distinct { "DISTINCT " } else { "" };
            let mut out = format!("{func_name}({distinct_str}{})", args_sql.join(", "));
            if let Some(f) = filter {
                out.push_str(" FILTER (WHERE ");
                out.push_str(&reconstruct_expr_sql(f));
                out.push(')');
            }
            out
        }
        Expr::UnaryOp {
            op, expr: inner, ..
        } => {
            let inner_sql = reconstruct_expr_sql(inner);
            match op {
                aiondb_parser::UnaryOperator::Not => format!("NOT ({inner_sql})"),
                aiondb_parser::UnaryOperator::Minus => format!("-{inner_sql}"),
                aiondb_parser::UnaryOperator::BitwiseNot => format!("~{inner_sql}"),
                aiondb_parser::UnaryOperator::Abs => format!("@{inner_sql}"),
                aiondb_parser::UnaryOperator::SquareRoot => format!("|/{inner_sql}"),
                aiondb_parser::UnaryOperator::CubeRoot => format!("||/{inner_sql}"),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => format!(
            "({} {} {})",
            reconstruct_expr_sql(left),
            reconstruct_binop_sql(op),
            reconstruct_expr_sql(right)
        ),
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            if *negated {
                format!("{} IS NOT NULL", reconstruct_expr_sql(inner))
            } else {
                format!("{} IS NULL", reconstruct_expr_sql(inner))
            }
        }
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} IS NOT DISTINCT FROM {}",
                    reconstruct_expr_sql(left),
                    reconstruct_expr_sql(right)
                )
            } else {
                format!(
                    "{} IS DISTINCT FROM {}",
                    reconstruct_expr_sql(left),
                    reconstruct_expr_sql(right)
                )
            }
        }
        Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let op = if *case_insensitive { "ILIKE" } else { "LIKE" };
            if *negated {
                format!(
                    "{} NOT {} {}",
                    reconstruct_expr_sql(inner),
                    op,
                    reconstruct_expr_sql(pattern)
                )
            } else {
                format!(
                    "{} {} {}",
                    reconstruct_expr_sql(inner),
                    op,
                    reconstruct_expr_sql(pattern)
                )
            }
        }
        Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => {
            let list_sql = list
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", ");
            if *negated {
                format!("{} NOT IN ({list_sql})", reconstruct_expr_sql(inner))
            } else {
                format!("{} IN ({list_sql})", reconstruct_expr_sql(inner))
            }
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} NOT BETWEEN {} AND {}",
                    reconstruct_expr_sql(inner),
                    reconstruct_expr_sql(low),
                    reconstruct_expr_sql(high)
                )
            } else {
                format!(
                    "{} BETWEEN {} AND {}",
                    reconstruct_expr_sql(inner),
                    reconstruct_expr_sql(low),
                    reconstruct_expr_sql(high)
                )
            }
        }
        Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => format!("CAST({} AS {})", reconstruct_expr_sql(inner), data_type),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut out = String::from("CASE");
            if let Some(op) = operand {
                out.push(' ');
                out.push_str(&reconstruct_expr_sql(op));
            }
            for (condition, result) in conditions.iter().zip(results.iter()) {
                out.push_str(" WHEN ");
                out.push_str(&reconstruct_expr_sql(condition));
                out.push_str(" THEN ");
                out.push_str(&reconstruct_expr_sql(result));
            }
            if let Some(el) = else_result {
                out.push_str(" ELSE ");
                out.push_str(&reconstruct_expr_sql(el));
            }
            out.push_str(" END");
            out
        }
        Expr::Array { elements, .. } => {
            let elements = elements
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", ");
            format!("ARRAY[{elements}]")
        }
        Expr::ArraySubquery { query, .. } => format!("ARRAY({})", reconstruct_select_sql(query)),
        Expr::Subquery { query, .. } => format!("({})", reconstruct_select_sql(query)),
        Expr::InSubquery {
            expr: inner,
            query,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} NOT IN ({})",
                    reconstruct_expr_sql(inner),
                    reconstruct_select_sql(query)
                )
            } else {
                format!(
                    "{} IN ({})",
                    reconstruct_expr_sql(inner),
                    reconstruct_select_sql(query)
                )
            }
        }
        Expr::Exists { query, negated, .. } => {
            if *negated {
                format!("NOT EXISTS ({})", reconstruct_select_sql(query))
            } else {
                format!("EXISTS ({})", reconstruct_select_sql(query))
            }
        }
        Expr::CypherExists { negated, .. } => {
            if *negated {
                "NOT EXISTS { ... }".to_owned()
            } else {
                "EXISTS { ... }".to_owned()
            }
        }
        Expr::CypherPatternComprehension { .. } => "[... | ...]".to_owned(),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            window_name,
            ..
        } => {
            let mut over_parts = Vec::new();
            if !partition_by.is_empty() {
                over_parts.push(format!(
                    "PARTITION BY {}",
                    partition_by
                        .iter()
                        .map(reconstruct_expr_sql)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !order_by.is_empty() {
                over_parts.push(format!(
                    "ORDER BY {}",
                    order_by
                        .iter()
                        .map(|item| {
                            let mut rendered = reconstruct_expr_sql(&item.expr);
                            if item.descending {
                                rendered.push_str(" DESC");
                            }
                            match item.nulls_first {
                                Some(true) => rendered.push_str(" NULLS FIRST"),
                                Some(false) => rendered.push_str(" NULLS LAST"),
                                None => {}
                            }
                            rendered
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(name) = window_name {
                over_parts.push(name.clone());
            }
            if over_parts.is_empty() {
                format!("{} OVER ()", reconstruct_expr_sql(function))
            } else {
                format!(
                    "{} OVER ({})",
                    reconstruct_expr_sql(function),
                    over_parts.join(" ")
                )
            }
        }
    }
}

pub fn reconstruct_select_sql(select: &aiondb_parser::SelectStatement) -> String {
    let mut sql = String::new();
    sql.push_str("SELECT ");

    match &select.distinct {
        aiondb_parser::DistinctKind::All => {}
        aiondb_parser::DistinctKind::Distinct => sql.push_str("DISTINCT "),
        aiondb_parser::DistinctKind::DistinctOn(exprs) => {
            sql.push_str("DISTINCT ON (");
            sql.push_str(
                &exprs
                    .iter()
                    .map(reconstruct_expr_sql)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            sql.push_str(") ");
        }
    }

    if select.items.is_empty() {
        sql.push('*');
    } else {
        let items = select
            .items
            .iter()
            .map(reconstruct_select_item_sql)
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&items);
    }

    if let Some(from) = &select.from {
        sql.push_str(" FROM ");
        sql.push_str(&from.parts.join("."));
        if let Some(alias) = &select.from_alias {
            sql.push(' ');
            sql.push_str(alias);
        }
    }

    for join in &select.joins {
        sql.push(' ');
        sql.push_str(&reconstruct_join_sql(join));
    }

    if let Some(selection) = &select.selection {
        sql.push_str(" WHERE ");
        sql.push_str(&reconstruct_expr_sql(selection));
    }

    if !select.group_by.is_empty() {
        sql.push_str(" GROUP BY ");
        sql.push_str(
            &select
                .group_by
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    if let Some(having) = &select.having {
        sql.push_str(" HAVING ");
        sql.push_str(&reconstruct_expr_sql(having));
    }

    if !select.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(
            &select
                .order_by
                .iter()
                .map(|item| {
                    let mut rendered = reconstruct_expr_sql(&item.expr);
                    if item.descending {
                        rendered.push_str(" DESC");
                    }
                    match item.nulls_first {
                        Some(true) => rendered.push_str(" NULLS FIRST"),
                        Some(false) => rendered.push_str(" NULLS LAST"),
                        None => {}
                    }
                    rendered
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    if let Some(limit) = &select.limit {
        sql.push_str(" LIMIT ");
        sql.push_str(&reconstruct_expr_sql(limit));
    }

    if let Some(offset) = &select.offset {
        sql.push_str(" OFFSET ");
        sql.push_str(&reconstruct_expr_sql(offset));
    }

    sql
}

fn reconstruct_select_item_sql(item: &aiondb_parser::SelectItem) -> String {
    let mut sql = reconstruct_expr_sql(&item.expr);
    if let Some(alias) = &item.alias {
        sql.push_str(" AS ");
        sql.push_str(alias);
    }
    sql
}

fn reconstruct_join_sql(join: &aiondb_parser::JoinClause) -> String {
    let mut sql = String::new();
    let join_keyword = match join.join_type {
        aiondb_parser::ast::JoinType::Inner => "JOIN",
        aiondb_parser::ast::JoinType::Left => "LEFT JOIN",
        aiondb_parser::ast::JoinType::Right => "RIGHT JOIN",
        aiondb_parser::ast::JoinType::Full => "FULL JOIN",
        aiondb_parser::ast::JoinType::Cross => "CROSS JOIN",
    };
    if join.natural {
        sql.push_str("NATURAL ");
    }
    sql.push_str(join_keyword);
    sql.push(' ');
    sql.push_str(&join.table.parts.join("."));
    if let Some(alias) = &join.alias {
        sql.push(' ');
        sql.push_str(alias);
    }
    if let Some(condition) = &join.condition {
        sql.push_str(" ON ");
        sql.push_str(&reconstruct_expr_sql(condition));
    }
    if !join.using_columns.is_empty() {
        sql.push_str(" USING (");
        sql.push_str(&join.using_columns.join(", "));
        sql.push(')');
        if let Some(using_alias) = &join.using_alias {
            sql.push_str(" AS ");
            sql.push_str(using_alias);
        }
    }
    sql
}

fn reconstruct_binop_sql(op: &aiondb_parser::BinaryOperator) -> &'static str {
    use aiondb_parser::BinaryOperator;
    match op {
        BinaryOperator::Add => "+",
        BinaryOperator::Exp => "^",
        BinaryOperator::BitwiseAnd => "&",
        BinaryOperator::BitwiseOr => "|",
        BinaryOperator::BitwiseXor => "#",
        BinaryOperator::ShiftLeft => "<<",
        BinaryOperator::ShiftRight => ">>",
        BinaryOperator::Sub => "-",
        BinaryOperator::Mul => "*",
        BinaryOperator::Div => "/",
        BinaryOperator::Mod => "%",
        BinaryOperator::Eq => "=",
        BinaryOperator::Ne => "!=",
        BinaryOperator::Lt => "<",
        BinaryOperator::Le => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::Ge => ">=",
        BinaryOperator::And => "AND",
        BinaryOperator::Or => "OR",
        BinaryOperator::Concat => "||",
        BinaryOperator::RegexMatch => "~",
        BinaryOperator::RegexMatchInsensitive => "~*",
        BinaryOperator::NotRegexMatch => "!~",
        BinaryOperator::NotRegexMatchInsensitive => "!~*",
        BinaryOperator::JsonGet => "->",
        BinaryOperator::JsonGetText => "->>",
        BinaryOperator::JsonPathGet => "#>",
        BinaryOperator::JsonPathGetText => "#>>",
        BinaryOperator::JsonContains => "@>",
        BinaryOperator::JsonContainedBy => "<@",
        BinaryOperator::JsonKeyExists => "?",
        BinaryOperator::JsonAnyKeyExists => "?|",
        BinaryOperator::JsonAllKeysExist => "?&",
        BinaryOperator::ArrayOverlap => "&&",
        BinaryOperator::FullTextSearch => "@@",
        BinaryOperator::JsonPathExists => "@?",
        BinaryOperator::GeometricEq => "~=",
        BinaryOperator::VectorL2Distance => "<->",
        BinaryOperator::VectorCosineDistance => "<=>",
        BinaryOperator::VectorNegativeInnerProduct => "<#>",
        BinaryOperator::VectorL1Distance => "<+>",
        BinaryOperator::VectorHammingDistance => "<~>",
        BinaryOperator::VectorJaccardDistance => "<%>",
    }
}

pub fn case_insensitive_replace_identifier(sql: &str, pattern: &str, replacement: &str) -> String {
    let sql_lower = sql.to_lowercase();
    let pattern_lower = pattern.to_lowercase();
    let mut result = String::new();
    let mut last = 0;
    for (idx, _) in sql_lower.match_indices(&pattern_lower) {
        let end = idx + pattern.len();
        let next_char = sql.get(end..end + 1).and_then(|s| s.chars().next());
        let is_boundary = next_char.map_or(true, |ch| !ch.is_alphanumeric() && ch != '_');
        if is_boundary {
            result.push_str(&sql[last..idx]);
            result.push_str(replacement);
            last = end;
        }
    }
    result.push_str(&sql[last..]);
    result
}

pub fn find_top_level_where(upper_sql: &str) -> Option<usize> {
    find_keyword_not_in_parens(upper_sql, "WHERE")
}

pub fn find_keyword_not_in_parens(upper_sql: &str, keyword: &str) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = upper_sql.as_bytes();
    let kw_bytes = keyword.as_bytes();
    let kw_len = kw_bytes.len();
    if bytes.len() < kw_len {
        return None;
    }
    for i in 0..=bytes.len() - kw_len {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'\'' => {}
            _ => {}
        }
        if depth == 0 && &bytes[i..i + kw_len] == kw_bytes {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after_ok = i + kw_len >= bytes.len() || !bytes[i + kw_len].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Some(i);
            }
        }
    }
    None
}
